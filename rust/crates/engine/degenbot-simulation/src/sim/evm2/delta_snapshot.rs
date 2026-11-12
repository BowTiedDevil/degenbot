//! `delta_snapshot` — the parent-linked delta-chain snapshot prototype: one
//! generation's snapshot is its parent's plus a small delta, and a bounded
//! chain of them is periodically compacted.
//!
//! The production carry-forward ([`ReadHubCache::advance_bal`](super::read_hub::ReadHubCache::advance_bal))
//! clones the parent's whole settled-storage map into every child. That is the
//! full-clone CONTROL this prototype is measured against, and it is unchanged:
//! this module is an additive alternative representation consumed by the
//! dispatch fan-out's development opt-in (`DEGENBOT_DELTA_DISPATCH`, whose
//! session driver fixes [`Retention::IdleBlocks`] at the measured threshold)
//! and by tests and examples. It answers the same transition questions — what a child
//! may serve, when a link is refused, what a final write does — with a
//! different snapshot shape.
//!
//! # The representation: child = parent + delta
//!
//! A [`DeltaSnapshot`] node is immutable and holds only its own generation's
//! values: the child's final writes and the values that generation's dynamic
//! reads settled. A lookup resolves the generation's own map first and only
//! then walks the parent chain, so a child's final write always shadows any
//! ancestor value and a value the child never touched is answered by the first
//! ancestor that holds it. Parents are immutable once linked: sealing a
//! generation MOVES its map into its node (no copy), and anything that
//! generation settles afterwards stays handle-local — exactly the visibility
//! boundary the control's clone-and-drop creates, where a parent hub's late
//! settles never reach a child that was already cloned.
//!
//! # Compaction bounds the walk
//!
//! [`DeltaSnapshotCache::with_max_depth`] sets the compaction threshold: no
//! node's [`depth`](DeltaSnapshot::depth) exceeds it. The transition that
//! would otherwise push past the threshold first folds its ancestors into one
//! compacted root (one map, [`Option::None`] parent), then links the new delta
//! to that. The fold builds fresh maps and never mutates an ancestor an older
//! reader still holds, preserves every value and the dynamic-fallback rule,
//! and is reported as its own cost ([`CompactionReport`]) instead of being
//! hidden inside the link. A lookup therefore walks the generation's own map
//! plus at most `max_depth + 1` node maps.
//!
//! # Parity obligations, and what this does not touch
//!
//! The transition is the control's, item for item: the exact-hash lineage
//! classification (the control's own `classify_link`, shared verbatim so the
//! two can never disagree), the child BAL fetched and fully decoded before anything links
//! (a missing, unsupported, RPC-failed, or malformed list yields a cold child
//! that serves no parent values), the write policy (a write to a known key
//! overwrites whatever `relevance` says; an unknown write seeds only when its
//! address is relevant and is left unknown otherwise), zero-write lists,
//! same-anchor idempotence, and cold children following the same new-write
//! rule. Only settled storage words cross between generations. Accounts and
//! EVM overlays keep the existing semantics untouched, and this prototype owns
//! no dispatch and no persistent-map/copy-on-write structure; those are
//! separate workstreams. One retention policy IS prototyped here, opt-in and
//! off by default (below), and one read seam IS: a chain miss registers with
//! the generation's own [`ReadHub`], so single-flight and batched rounds are
//! the production hub's, never a second implementation (below).
//!
//! # The read seam: chain hits locally, misses through the hub
//!
//! [`DeltaHub::read_storage_handle`] is the candidate read: the generation's
//! chain answers first ([`ReadOrigin::Snapshot`], no RPC), and anything it
//! cannot answer — an unknown identity, or an entry the retention policy has
//! expired — registers with the generation's [`ReadHub`] ([`ReadOrigin::Hub`]).
//! One hub per generation, minted at that generation's anchor, so duplicate
//! misses coalesce onto one member, distinct misses share one storage batch per
//! drained round, and no memoized value is reachable from another anchor.
//! Accounts read the same hub ([`DeltaHub::read_account`]) and are never
//! carried.
//!
//! A successful round settles its answer into the generation that asked
//! (`DeltaHub::settle`): the value joins that generation's own map, which
//! shadows every ancestor and travels into the next generation at seal, and the
//! candidate use is stamped with the READING generation's block — once per
//! resolving handle, so a coalesced duplicate cannot extend a window. A refused
//! member and an abandoned round settle nothing, and the identity is readable
//! again on the next round. A settle that lands after a child linked the
//! generation stays in that handle's late map, invisible to the parent and to
//! every child, because the sealed node is immutable.
//!
//! # Idle-block retention (opt-in)
//!
//! [`Retention::IdleBlocks`] expires entries candidates have not actually
//! read: every entry carries the block of its last USE, and the policy stops
//! serving it once it has survived `idle` blocks without a candidate read —
//! used at block `u`, visible through `u + idle`, gone from `u + idle + 1` on.
//! [`DeltaSnapshotCache::with_retention`] opts a session in;
//! [`Retention::Unbounded`] (the default of [`DeltaSnapshotCache::new`]) is the
//! unbounded delta control.
//!
//! What counts as use, and what does not:
//!
//! - A CANDIDATE READ ([`DeltaHub::read_storage`]) is use: it stamps the
//!   entry's record with the reading generation, whether the answer came from
//!   the chain or the dynamic fallback. [`DeltaHub::snapshot_value`] is an
//!   inspection, not a candidate read, and refreshes nothing.
//! - A BAL WRITE is never use. An overwrite of a still-served entry carries
//!   the entry's record forward (the write changed the value, not the use), so
//!   an entry candidates stop reading expires on its original schedule however
//!   often the list rewrites it. A write that SEEDS an entry — an unknown
//!   identity, or an expired one back under the relevance rule (expired is
//!   unknown) — gives it a birth at the writing block: one idle window unless
//!   a candidate reads it. The write policy itself is unchanged: a write to a
//!   still-served key overwrites whatever `relevance` says.
//!
//! Expiry is invisibility, never staleness: an expired entry resolves as
//! unknown through every ancestor and after every compaction, so a read of it
//! takes the ordinary dynamic fallback and gets the chain's CURRENT word at
//! the reader's own anchor. Compaction preserves each surviving entry's record
//! (a fold gives no entry a new life) and drops expired values together with
//! their metadata. Parent/child isolation is unchanged: visibility is judged
//! at each handle's OWN anchor, so a child's clock never expires anything out
//! of an older handle's view.
//!
//! # The asynchronous canonical-head lifecycle
//!
//! [`BalHeadDriver`] wraps one cache with the transition a head stream needs. A
//! head arrives ([`BalHeadDriver::deliver`]) with its exact anchor and the parent
//! hash from its header — and NO relevance: a head comes off the block clock,
//! which has no candidates, so the set only has to exist when the head's writes
//! are APPLIED. The driver records the head and mints ONE cold generation that
//! every caller at that head reads until the apply lands. Its BAL is fetched and
//! fully decoded separately ([`PreparedBal`], [`BalHeadDriver::prepare`]) — the
//! stage that owns the RPC, so a caller runs it off its critical path — and
//! applied in one synchronous transition ([`BalHeadDriver::apply`]) that is
//! [`DeltaSnapshotCache::advance_bal`] with the fetch already done: same lineage
//! classification, same sealed parent chain, same write policy, same retention
//! and compaction transition.
//!
//! The relevance a head's unknown keys seed under is the apply's argument, and
//! [`BalHeadDriver::advance_with_relevance`] resolves it at the LATEST point the
//! transition allows: after the list landed, immediately before the apply. A
//! registration that committed during the fetch window is therefore included in
//! this head's seeding, and one that commits after the apply seeds from the next
//! head — without ever making a served value wrong, because relevance only gates
//! seeding an unknown key.
//!
//! Readiness is keyed by the WHOLE anchor, never the number. A caller at a
//! delivered head gets [`HeadReadiness::Cold`] with the reason until that exact
//! head's list applied — so an unprepared head, a list the node cannot serve, and
//! a malformed list all take the cold path instead of ever serving a parent
//! value at the wrong anchor. Once settled the head is [`HeadReadiness::Ready`],
//! or cold-and-settled when the lineage refused or the list could not be read
//! (the two ways the synchronous control also settles cold).
//!
//! Readiness is decided ONCE, by the settle that produced it. Delivering a head
//! whose anchor is already installed (a reorg back to the installed block)
//! repeats no transition and therefore keeps that generation's settle-derived
//! readiness: a head that settled cold answers cold with its exact reason after
//! the redelivery too, never `Ready`. A caller's diagnostic line and its metric
//! labels therefore never claim a carry that did not happen.
//!
//! Three fences keep a late or wrong response out of the snapshot: the prepared
//! list must carry the ticket's exact anchor ([`HeadOutcome::Foreign`]), the
//! ticket must still be the driver's head ([`HeadOutcome::Superseded`]), and the
//! transition itself re-classifies lineage against whatever the cache has
//! installed. A head that a same-height reorg, a gap, or a duplicate delivery
//! replaced therefore installs nothing, and the abandoned branch's values never
//! resurrect: the next head above the survivor is consecutive again and carries
//! only the survivor's own generation.
//!
//! [`BalHeadDriver::snapshot`] is synchronous, so a candidate loop never blocks
//! on a BAL; [`BalHeadDriver::advance`] is the claim-and-fetch form a caller
//! spawns, it tells a second concurrent caller [`HeadOutcome::InFlight`] instead
//! of duplicating the fetch, and a cancelled advance releases its claim so the
//! head prepares normally next time.
//!
//! A caller that wants the head's SETTLED answer joins that existing
//! preparation instead of falling back while it runs:
//! [`BalHeadDriver::wait_for_head`] registers on the exact `(anchor, seq)`
//! generation's current preparation attempt — while it is genuinely live — and
//! waits for that attempt's completion notification ([`HeadCompletion`]), then
//! re-reads the exact anchor. Completion is scoped to the ATTEMPT: a retry after
//! an abandoned attempt mints its own private epoch, so abandoning attempt N
//! stays terminal for N's waiters even when N+1 settles the same anchor before
//! they poll. The wait is bounded by real outcomes, never by a wall clock:
//! a settle (a provider failure settling cold included), a supersession, or
//! the preparation's abandonment — a generation no live preparation owns
//! answers [`HeadCold::Abandoned`] and the caller takes its fallback lane, so
//! nothing is ever parked on a preparation that is gone. The wait fetches
//! nothing, claims nothing, and holds no lock across it. Every terminal path
//! wakes its waiters, so no registration is stranded, and many waiters share
//! one attempt's one completion.
//!
//! Two costs are deliberate and visible. The provisional generation is cold, so
//! whatever it settles before the apply is dropped with it — a candidate that
//! reads the same identity again pays one more dynamic read, and the settled
//! generation never inherits a value it did not earn. And the driver keeps one
//! head at a time: a caller holding a view keeps the generation it was handed
//! (the sealed node is immutable), while a query for a superseded or never
//! delivered anchor mints a fresh cold generation per call.
//!
//! # Migration onto the lifecycle
//!
//! A consumer moving onto the lifecycle keeps the cache's semantics and changes
//! only WHO owns the clock. The synchronous [`DeltaSnapshotCache::advance_bal`]
//! remains the control — one awaited call that fetches, applies, and installs —
//! and [`BalHeadDriver`] is the same transition split across a head stream:
//!
//! 1. own the driver for the session instead of the cache
//!    ([`BalHeadDriver::new`] / [`BalHeadDriver::with_config`], which fixes the
//!    compaction threshold and the retention policy);
//! 2. on every canonical head, [`BalHeadDriver::deliver`] the anchor, the
//!    header's parent hash, and the relevance set the caller derived — the
//!    relevance input stays the CALLER's (production derivation is not part of
//!    this seam);
//! 3. run [`BalHeadDriver::prepare`] (or the whole [`BalHeadDriver::advance`])
//!    wherever the caller keeps its background work, and read every identity
//!    through [`BalHeadDriver::snapshot`]'s view — never through the parent
//!    generation — so a head that is not ready cannot answer;
//! 4. branch on [`HeadReadiness`]: `Ready` means the head's generation carries
//!    its chain, `Cold(reason)` means the caller is on the existing cold path
//!    and the reason is available for its own telemetry
//!    ([`BalHeadDriver::stats`] counts the views).
//!
//! Nothing else changes: the write policy, zero-write lists, same-anchor
//! idempotence, retention expiry, parent/child isolation, and the compaction
//! bound are the ones the cache already had, and the existing delta suites are
//! the evidence. Dispatch integration, production relevance derivation, and any
//! retention redesign remain separate workstreams — this seam ends at the view.
//! The live measurement of the stages is `just bal-head-lifecycle-probe`, and its
//! report is `logs/bal_head_lifecycle_result.md`.
//!
//! # Development and migration notes
//!
//! A consumer opting in swaps its snapshot owner for a [`DeltaSnapshotCache`]
//! and its per-anchor read handle for a [`DeltaHub`]: `for_anchor` and
//! `advance_bal` keep the signatures and semantics the hub cache taught it
//! (same [`ReadAnchor`] pinning, same [`CarryOutcome`](super::read_hub::CarryOutcome)
//! / [`CarryRefusal`](super::read_hub::CarryRefusal) reports). Retention adds
//! ONE configuration decision and no new seam: pick [`Retention::IdleBlocks`]
//! with a threshold ([`DeltaSnapshotCache::with_retention`] /
//! [`DeltaSnapshotCache::with_max_depth_and_retention`]) and every read, write,
//! and compaction keeps its meaning.
//!
//! The read side has two entry points, and a consumer picks by who owns the
//! round:
//!
//! - [`DeltaHub::read_storage`] is the sequential convenience: register, drain
//!   one round, await. Its error type is the hub's
//!   ([`ReadHubError`](super::read_hub::ReadHubError)), so a refused member and
//!   an abandoned round are distinguishable, and nothing is settled on either.
//! - [`DeltaHub::read_storage_handle`] is the scheduler seam: register every
//!   read, poll the fibers, and [`DeltaHub::drain_round`] when none can
//!   progress — the same shape as [`ReadHub::read_storage`](super::read_hub::ReadHub::read_storage)
//!   plus [`ReadHub::drain_round`](super::read_hub::ReadHub::drain_round). The
//!   recorded-read driver (`examples/shared/session_drive.rs`) drives a delta
//!   generation and the full-clone control through one lane abstraction, which
//!   is what makes a round-coalesced replay comparable to the existing legs.
//!
//! [`DeltaHub::read_hub`] exposes the generation's hub for a caller that owns
//! the round coordinator (the driver's [`HubDrain`](super::async_fanout::HubDrain)),
//! and [`DeltaHub::read_stats`] reports the round counters (rounds, distinct
//! members, memo hits, coalesced duplicates) the replay's fetch accounting
//! reads.
//!
//! What the probes measure, and what a production integration still owes:
//! whether the delta chain beats a full clone at realistic working-set sizes
//! (`just bal-delta-snapshot-probe`, knobs `BAL_DELTA_SIZES`, `BAL_DELTA_REPS`,
//! `BAL_DELTA_LOOKUPS`, `BAL_DELTA_PLANT_DIVERGENCE`), and which idle-block
//! threshold (if any) bounds memory without burning the reuse the carry exists
//! for (`just bal-retention-probe`, knobs `BAL_RETENTION_THRESHOLDS`,
//! `BAL_RETENTION_SYNTH_BLOCKS`, `BAL_RETENTION_SYNTH_KEYS`,
//! `BAL_RETENTION_SYNTH_WAVE`, `BAL_RETENTION_PLANT_DIVERGENCE`). The
//! round-coalesced scheduler replay (`just bal-delta-rounds-probe`, knobs
//! `BAL_DELTA_ROUNDS_TRACES`, `BAL_DELTA_ROUNDS_FIXTURES`,
//! `BAL_DELTA_ROUNDS_STORAGE_US`, `BAL_DELTA_ROUNDS_ACCOUNT_US`,
//! `BAL_DELTA_ROUNDS_IDLE`, `BAL_DELTA_ROUNDS_PLANT_DIVERGENCE`) adds the
//! candidate re-read gap distributions and the measured idle-window cost. The
//! representation verdict is in `logs/bal_delta_snapshot_result.md`, the
//! retention tradeoff in `logs/bal_retention_result.md`, and the round-coalesced
//! replay's numbers, retention decision, and GO/NO-GO in
//! `logs/bal_delta_rounds_result.md`. Production relevance derivation, the
//! new-head lifecycle, and dispatch integration remain separate workstreams —
//! the seam here is a scheduler-shaped read, not a dispatch.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use std::time::{Duration, Instant};

use std::future::Future;

use std::pin::Pin;

use std::task::{Context, Poll};

use alloy::primitives::{Address, StorageValue, B256, U256};

use parking_lot::Mutex;
use tokio::sync::watch;

use degenbot_rpc::provider::{AccountRead, AlloyProvider};

use super::bal_prefetch::BalSkip;

use super::read_hub::{
    classify_link, CarryOutcome, CarryRefusal, PreparedBal, ReadFuture, ReadHub, ReadHubError,
    ReadHubStats, RoundReport,
};
use super::storage_view::StorageView;

use crate::sim::storage_memo::ReadAnchor;

/// One storage read identity: one word of one account at a generation's
/// anchor.
pub type Slot = (Address, U256);

/// The default compaction threshold: the ancestor links any one node may
/// carry before its next child compacts them into one root.
pub const DEFAULT_MAX_DEPTH: u32 = 8;

/// The retention policy a [`DeltaSnapshotCache`] applies to its chain: what a
/// candidate read keeps alive, and for how long (see the module docs for what
/// counts as use).
///
/// The clock is BLOCKS, never wall time or lookup counts: an entry records the
/// block of its last use, and the policy is asked whether that entry is still
/// served at the generation asking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Retention {
    /// No expiry: every settled word is retained until its generation is
    /// retired — the unbounded delta control.
    #[default]
    Unbounded,
    /// Idle-block expiry: an entry used at block `last_used` survives `idle`
    /// blocks without a candidate read — visible through `last_used + idle`,
    /// gone from `last_used + idle + 1` on. A threshold of 0 serves an entry
    /// only in the generation that used it.
    IdleBlocks(u64),
}

impl Retention {
    /// Whether an entry last used at `last_used` is still served at `now`.
    #[must_use]
    pub const fn serves(self, last_used: u64, now: u64) -> bool {
        match self {
            Self::Unbounded => true,
            Self::IdleBlocks(idle) => now.saturating_sub(last_used) <= idle,
        }
    }
}

/// One immutable generation node: this generation's delta over its parent
/// chain.
///
/// A node's [`delta`](Self::delta_len) holds exactly what its generation
/// contributed — final writes applied at the transition and values its dynamic
/// reads settled — never a copy of an ancestor's map. [`Self::get`] resolves
/// the delta before the ancestors, which is the final-write precedence the
/// full-clone control gets from one flat map. The `used` map is recency
/// metadata: every delta identity has a record there (its birth, or the record
/// an overwrite carried forward) plus a record for every identity this
/// generation READ whose value an ancestor holds — a refresh with no value of
/// its own.
pub struct DeltaSnapshot {
    parent: Option<Arc<DeltaSnapshot>>,

    delta: HashMap<Slot, StorageValue>,

    used: HashMap<Slot, u64>,

    depth: u32,

    entries: usize,
}

impl DeltaSnapshot {
    /// The void root a fresh generation links against: no values, no ancestors.
    fn root() -> Self {
        Self {
            parent: None,

            delta: HashMap::new(),

            used: HashMap::new(),

            depth: 0,

            entries: 0,
        }
    }

    /// A node over `parent` carrying `delta`, of which `new_keys` identities
    /// the parent chain does not already hold.
    fn node(
        parent: Option<Arc<DeltaSnapshot>>,

        delta: HashMap<Slot, StorageValue>,

        used: HashMap<Slot, u64>,

        new_keys: usize,
    ) -> Self {
        let (depth, base_entries) = parent
            .as_ref()
            .map_or((0, 0), |node| (node.depth.saturating_add(1), node.entries));

        Self {
            parent,

            delta,

            used,

            depth,

            entries: base_entries.saturating_add(new_keys),
        }
    }

    /// Resolve one identity through this node and its ancestors under
    /// `retention` at generation `now` — the child's own writes and settles
    /// before any ancestor value, and [`Option::None`] for an entry the policy
    /// has expired (which is unknown again: the dynamic fallback owns it).
    #[must_use]
    pub fn get(&self, key: &Slot, now: u64, retention: Retention) -> Option<StorageValue> {
        let last_used = self.freshest_use(key)?;

        if !retention.serves(last_used, now) {
            return None;
        }

        self.held_value(key)
    }

    /// The value this chain holds for the identity, ignoring retention — the
    /// youngest generation's value wins.
    fn held_value(&self, key: &Slot) -> Option<StorageValue> {
        if let Some(value) = self.delta.get(key) {
            return Some(*value);
        }

        self.parent
            .as_ref()
            .and_then(|parent| parent.held_value(key))
    }

    /// The freshest recency record anywhere in this chain for the identity.
    ///
    /// A record is written at its generation's block and a younger node's
    /// record is never staler than an ancestor's — but a compacted root merges
    /// records of many generations, so the walk takes the MAXIMUM rather than
    /// trusting the order.
    fn freshest_use(&self, key: &Slot) -> Option<u64> {
        let own = self.used.get(key).copied();

        let below = self
            .parent
            .as_ref()
            .and_then(|parent| parent.freshest_use(key));

        match (own, below) {
            (Some(own), Some(below)) => Some(own.max(below)),
            (own, below) => own.or(below),
        }
    }

    /// Whether this chain holds a value for the identity, ignoring retention.
    fn holds(&self, key: &Slot) -> bool {
        self.held_value(key).is_some()
    }

    /// Every identity this chain holds a value for.
    fn collect_value_keys(&self, keys: &mut HashSet<Slot>) {
        keys.extend(self.delta.keys().copied());

        if let Some(parent) = self.parent.as_deref() {
            parent.collect_value_keys(keys);
        }
    }

    /// The ancestor links a lookup at this node walks below its own map.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// How many identities this node's own delta holds — never an ancestor's
    /// copy.
    #[must_use]
    pub fn delta_len(&self) -> usize {
        self.delta.len()
    }

    /// Distinct identities this chain holds a value for: its own delta plus
    /// every ancestor's. Under retention some may already be expired at a
    /// later generation — [`DeltaHub::live_len`] counts what is still served.
    #[must_use]
    pub const fn entries(&self) -> usize {
        self.entries
    }

    /// Whether this is the void root: nothing to compact, nothing to link.
    fn is_void(&self) -> bool {
        self.parent.is_none() && self.delta.is_empty() && self.used.is_empty()
    }

    /// Fold this node's whole ancestor chain into one compacted root: a fresh
    /// map with [`Option::None`] parent, built oldest-generation-first so a
    /// later generation's value and recency record win. Every ancestor keeps
    /// its own maps — an older reader holding any of them is untouched.
    ///
    /// Under retention the fold drops every identity the policy has expired at
    /// `now` (value and metadata together) and preserves each survivor's
    /// record: compaction gives no entry a new life.
    fn compact(&self, now: u64, retention: Retention) -> (Arc<DeltaSnapshot>, CompactionReport) {
        let started = Instant::now();

        let mut chain = Vec::new();

        let mut walk = Some(self);

        while let Some(node) = walk {
            chain.push(node);

            walk = node.parent.as_deref();
        }

        let mut merged = HashMap::with_capacity(self.entries);

        let mut merged_used: HashMap<Slot, u64> = HashMap::with_capacity(self.entries);

        for node in chain.iter().rev() {
            merged.extend(node.delta.iter().map(|(key, value)| (*key, *value)));

            merged_used.extend(node.used.iter().map(|(key, last_used)| (*key, *last_used)));
        }

        let expired: Vec<Slot> = merged_used
            .iter()
            .filter(|(_, last_used)| !retention.serves(**last_used, now))
            .map(|(key, _)| *key)
            .collect();

        for key in &expired {
            merged_used.remove(key);

            merged.remove(key);
        }

        let entries = merged.len();

        let nodes_merged = chain.len();

        let root = Arc::new(DeltaSnapshot::node(None, merged, merged_used, entries));

        (
            root,
            CompactionReport {
                nodes_merged,

                entries,

                dropped: expired.len(),

                elapsed: started.elapsed(),
            },
        )
    }
}

/// What one compaction fold cost — reported apart from the link so the two
/// costs never blur together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionReport {
    /// Chain nodes folded into the compacted root.
    pub nodes_merged: usize,

    /// Distinct identities the compacted root holds.
    pub entries: usize,

    /// Identities dropped as expired — value and recency metadata together.
    /// [`Retention::Unbounded`] drops nothing.
    pub dropped: usize,

    /// Wall time of the fold.
    pub elapsed: Duration,
}

/// What one application of a child's final writes did.
struct AppliedWrites {
    /// The identities applied, in BAL order.
    applied: Vec<(Address, U256)>,

    /// How many applied writes took the SERVED-key overwrite path. Under
    /// retention a write to an expired-but-held identity re-seeds instead
    /// (module docs) and is counted in neither `overwritten` nor `new_keys`.
    overwritten: usize,

    /// Unknown-slot writes left unknown: address outside the relevance set.
    skipped: usize,

    /// Applied writes to identities no ancestor chain holds.
    new_keys: usize,
}

/// The identities one head's unknown-slot writes may SEED, as MEMBERSHIP.
///
/// The write policy asks one question per BAL write — "is this write's address
/// relevant?" — and answers it with a hash probe, never a scan: a live head's
/// list carries on the order of 1.4 k writes and the session's set is bounded by
/// the whole path registry, so a linear `contains` would spend 10⁶–10⁷ address
/// comparisons on the apply path.
///
/// Two construction forms, ONE behaviour (pinned by
/// `the_set_and_slice_relevance_inputs_produce_an_identical_report`):
///
/// - [`Self::from_slice`] converts a caller's address list once, before any
///   write is examined — a per-fan-out derivation
///   ([`path_relevance_addresses`](crate::sim::prefetch::path_relevance_addresses)),
///   and the synchronous [`DeltaSnapshotCache::advance_bal`] callers;
/// - [`Self::from_set`] hands over a membership the caller already materialized
///   — the session's apply-time relevance, derived once per registry revision
///   (see [`HeadRelevanceSource`](super::bal_heads::HeadRelevanceSource)).
///
/// Relevance only ever gates SEEDING an unknown key: a write to a key the
/// generation still serves overwrites whatever the set says, and a skipped
/// write's read takes the ordinary dynamic path at the reader's own anchor. A
/// WIDER set therefore costs memory, never a value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelevanceSet {
    addresses: HashSet<Address>,
}

impl RelevanceSet {
    /// The membership `addresses` names — duplicates collapse, order is
    /// irrelevant (nothing downstream reads a sequence).
    #[must_use]
    pub fn from_slice(addresses: &[Address]) -> Self {
        Self {
            addresses: addresses.iter().copied().collect(),
        }
    }

    /// A membership the caller already materialized.
    #[must_use]
    pub fn from_set(addresses: HashSet<Address>) -> Self {
        Self { addresses }
    }

    /// Whether `address`'s unknown slots may seed.
    #[must_use]
    pub fn contains(&self, address: &Address) -> bool {
        self.addresses.contains(address)
    }

    /// How many identities the set names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.addresses.len()
    }

    /// Whether the set names nothing — a session with no registered path and no
    /// settled identity (§4.5 of the design), which seeds nothing of its own.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }
}

impl FromIterator<Address> for RelevanceSet {
    fn from_iter<I: IntoIterator<Item = Address>>(addresses: I) -> Self {
        Self {
            addresses: addresses.into_iter().collect(),
        }
    }
}

/// Apply the child BAL's final writes to a fresh delta over `base` under the
/// carry's write policy: a write to a slot the snapshot still serves
/// OVERWRITES it whatever `relevance` says (a stale carried value outliving
/// the block that changed it is the one outcome the carry must never produce,
/// a zero or deletion final included), and a write to an UNKNOWN slot seeds
/// only when its address is in `relevance` — an irrelevant unknown write stays
/// unknown (counted, never hidden) so a later read takes the dynamic path.
/// Under retention an expired entry IS unknown (see the module docs): the
/// write re-seeds under the same relevance rule and births a fresh window.
///
/// `relevance` is MEMBERSHIP ([`RelevanceSet`]): the per-write question is one
/// hash probe, whatever shape the caller's own derivation had.
///
/// A write is never candidate use: an overwrite carries the entry's old
/// recency record forward, and only a seeding write stamps a birth.
fn seed_final_writes(
    writes: &[(Address, U256, U256)],

    relevance: &RelevanceSet,

    base: &DeltaSnapshot,

    now: u64,

    retention: Retention,
) -> (
    HashMap<Slot, StorageValue>,
    HashMap<Slot, u64>,
    AppliedWrites,
) {
    let mut values = HashMap::new();

    let mut used = HashMap::new();

    let mut applied_writes = AppliedWrites {
        applied: Vec::new(),

        overwritten: 0,

        skipped: 0,

        new_keys: 0,
    };

    for &(address, slot, value) in writes {
        let key = (address, slot);

        let held = values.contains_key(&key) || base.holds(&key);

        let served = values.contains_key(&key) || base.get(&key, now, retention).is_some();

        if !served && !relevance.contains(&address) {
            applied_writes.skipped = applied_writes.skipped.saturating_add(1);

            continue;
        }

        let last_used = if served {
            used.get(&key)
                .copied()
                .or_else(|| base.freshest_use(&key))
                .unwrap_or(now)
        } else {
            now
        };

        values.insert(key, value);

        used.insert(key, last_used);

        if served {
            applied_writes.overwritten = applied_writes.overwritten.saturating_add(1);
        } else if !held {
            applied_writes.new_keys = applied_writes.new_keys.saturating_add(1);
        }

        applied_writes.applied.push(key);
    }

    (values, used, applied_writes)
}

/// One generation's state: its own values over the chain below them.
struct GenState {
    /// This generation's own values — its delta while it is the chain head,
    /// its handle-local late settles once a child has sealed it.
    values: HashMap<Slot, StorageValue>,

    /// Recency records for identities this generation used: candidate reads
    /// (including reads of values an ancestor holds), and write births. Every
    /// `values` identity has a record here.
    used: HashMap<Slot, u64>,

    /// The chain below `values`: the base ancestors before sealing, this
    /// generation's own sealed node afterwards.
    node: Arc<DeltaSnapshot>,

    /// Whether `node` already includes `values` (a child linked and sealed
    /// this generation), which makes `values` the late map.
    sealed: bool,

    /// Identities in `values` the chain below does not hold.
    new_keys: usize,
}

impl GenState {
    /// Resolve one identity: this generation's own map before the chain, and
    /// [`Option::None`] for an entry the policy has expired.
    ///
    /// [`Retention::Unbounded`] serves every record at every block, so recency
    /// cannot change a value: the answer is the value lookup alone. Every held
    /// identity carries a record (see [`Self::values`]), which is all the
    /// policy path's record probe can find, so skipping it skips no expiry.
    fn resolved(&self, key: &Slot, now: u64, retention: Retention) -> Option<StorageValue> {
        match retention {
            Retention::Unbounded => self
                .values
                .get(key)
                .copied()
                .or_else(|| self.node.held_value(key)),
            Retention::IdleBlocks(_) => self.resolved_through_policy(key, now, retention),
        }
    }

    /// The general read path: the freshest recency record this generation and
    /// its chain hold, the policy's question about it, and only then the value
    /// — the reference [`Self::resolved`]'s unbounded arm is pinned against
    /// (`the_unbounded_read_answers_exactly_what_the_policy_path_answers`).
    fn resolved_through_policy(
        &self,

        key: &Slot,

        now: u64,

        retention: Retention,
    ) -> Option<StorageValue> {
        let own = self.used.get(key).copied();

        let below = self.node.freshest_use(key);

        let last_used = match (own, below) {
            (Some(own), Some(below)) => Some(own.max(below)),
            (own, below) => own.or(below),
        }?;

        if !retention.serves(last_used, now) {
            return None;
        }

        self.values
            .get(key)
            .copied()
            .or_else(|| self.node.held_value(key))
    }

    /// Record one candidate read's use at `now` — the refresh a chain hit and
    /// a dynamic settle share.
    fn touch(&mut self, key: Slot, now: u64) {
        self.used.insert(key, now);
    }

    /// Record one settled value, counting it only when neither this
    /// generation's map nor the chain below already held the identity.
    fn settle(&mut self, key: Slot, value: StorageValue, now: u64) {
        let fresh = !self.values.contains_key(&key) && !self.node.holds(&key);

        self.values.insert(key, value);

        self.used.insert(key, now);

        if fresh {
            self.new_keys = self.new_keys.saturating_add(1);
        }
    }

    /// The identities this state can serve at `now`: every held value whose
    /// freshest record the policy still serves.
    ///
    /// [`Retention::Unbounded`] expires nothing, so the answer is the chain's
    /// own identity count — the arithmetic [`DeltaHub::snapshot_len`] reports —
    /// instead of collecting every chain key and re-resolving each one.
    fn live_len(&self, now: u64, retention: Retention) -> usize {
        match retention {
            Retention::Unbounded => self.node.entries().saturating_add(self.new_keys),
            Retention::IdleBlocks(_) => self.live_len_through_policy(now, retention),
        }
    }

    /// The general live count: every identity this generation and its chain
    /// hold, counted only when the policy still serves it.
    fn live_len_through_policy(&self, now: u64, retention: Retention) -> usize {
        let mut keys: HashSet<Slot> = self.values.keys().copied().collect();

        self.node.collect_value_keys(&mut keys);

        keys.into_iter()
            .filter(|key| self.resolved_through_policy(key, now, retention).is_some())
            .count()
    }
}

/// One anchor generation's snapshot handle (see the module docs).
///
/// Reads resolve the generation's own map before its ancestors; a hit is the
/// whole answer, and a miss registers with this generation's own [`ReadHub`],
/// so every fetch takes the production hub's single-flight, batched round (see
/// [`Self::read_storage_handle`]). A settled value joins the generation's own
/// map, so it shadows ancestors and carries to the next generation exactly like
/// the control's memo.
pub struct DeltaHub {
    anchor: ReadAnchor,

    retention: Retention,

    /// This generation's read lane: the round machinery every chain miss
    /// registers with, and the account memo candidates share. One hub per
    /// generation, minted at the generation's own anchor, so no memoized value
    /// is reachable from another anchor.
    reads: ReadHub,

    state: Mutex<GenState>,
}

impl DeltaHub {
    /// A generation whose own state starts at `state`.
    fn generation(
        provider: &AlloyProvider,

        anchor: ReadAnchor,

        retention: Retention,

        state: GenState,
    ) -> Self {
        Self {
            anchor,

            retention,

            reads: ReadHub::new(provider, anchor),

            state: Mutex::new(state),
        }
    }

    /// A fresh cold generation: nothing carried, nothing written.
    fn fresh(provider: &AlloyProvider, anchor: ReadAnchor, retention: Retention) -> Self {
        Self::generation(
            provider,
            anchor,
            retention,
            GenState {
                values: HashMap::new(),

                used: HashMap::new(),

                node: Arc::new(DeltaSnapshot::root()),

                sealed: false,

                new_keys: 0,
            },
        )
    }

    /// The anchor every value this handle serves is pinned to.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The snapshot's own answer for one identity — [`Option::None`] while the
    /// identity is still unknown to this generation and every ancestor, and
    /// for an entry the retention policy has expired. A lookup, not a
    /// candidate read: it refreshes no recency.
    #[must_use]
    pub fn snapshot_value(&self, address: Address, index: U256) -> Option<StorageValue> {
        let state = self.state.lock();

        state.resolved(&(address, index), self.anchor.number, self.retention)
    }

    /// Distinct identities the snapshot holds: this generation's own map plus
    /// every ancestor's, the control's `memoized_slots` counterpart. Under
    /// retention the count includes entries already expired at this
    /// generation — [`Self::live_len`] counts what is still served.
    #[must_use]
    pub fn snapshot_len(&self) -> usize {
        let state = self.state.lock();

        state.node.entries().saturating_add(state.new_keys)
    }

    /// Distinct identities this handle can actually serve at its generation:
    /// [`Self::snapshot_len`] minus every entry the retention policy has
    /// expired (equal to it under [`Retention::Unbounded`]).
    #[must_use]
    pub fn live_len(&self) -> usize {
        let state = self.state.lock();

        state.live_len(self.anchor.number, self.retention)
    }

    /// How many identities this generation itself contributed VALUES for — its
    /// own delta while it is the head, its sealed delta plus its late settles
    /// after a child sealed it. A recency-only record (a read of a value an
    /// ancestor holds) is metadata, not a delta value. A delta child near this
    /// number and a `snapshot_len` near its parent's is the no-full-copy shape.
    #[must_use]
    pub fn delta_len(&self) -> usize {
        let state = self.state.lock();

        let sealed_delta = if state.sealed {
            state.node.delta_len()
        } else {
            0
        };
        sealed_delta + state.values.len()
    }

    /// The node maps a lookup at this handle walks below its own map — bounded
    /// by the cache's compaction threshold plus the compacted root.
    #[must_use]
    pub fn chain_depth(&self) -> u32 {
        let state = self.state.lock();

        if state.node.is_void() {
            0
        } else {
            state.node.depth().saturating_add(1)
        }
    }

    /// The generation's read lane: the [`ReadHub`] every chain miss registers
    /// with and every account read is answered by. A caller that owns the round
    /// machinery (a scheduler) takes the hub directly and drains it; a caller
    /// that just wants the value uses [`Self::read_storage`].
    #[must_use]
    pub fn read_hub(&self) -> &ReadHub {
        &self.reads
    }

    /// Drain one round of this generation's read lane: every queued miss takes
    /// one storage batch (and every queued account its own batch), the answers
    /// settle, and the read handles they belong to resolve. An empty report
    /// means nothing was queued — another round already owns every pending read.
    pub async fn drain_round(&self) -> RoundReport {
        self.reads.drain_round().await
    }

    /// This generation's cumulative round counters: rounds drained, distinct
    /// members dispatched, memo hits, and coalesced duplicate reads. The delta's
    /// fetch economy is visible here, not inferred from a wire shape.
    #[must_use]
    pub fn read_stats(&self) -> ReadHubStats {
        self.reads.stats()
    }

    /// Register one storage read at this generation's anchor — the candidate-read
    /// seam a scheduler uses: a chain hit resolves on its first poll without any
    /// RPC, and a miss registers with this generation's [`ReadHub`] so duplicate
    /// misses coalesce onto one member and distinct misses share one storage
    /// batch. Whatever a read answers stamps the entry's recency with this
    /// generation's block, once the handle resolves.
    ///
    /// An entry the retention policy has expired is a miss like any other: its
    /// round answers the chain's CURRENT word and the entry re-settles with a
    /// fresh window. Errors never settle: a refused member and an abandoned
    /// round leave the generation exactly as it was, and the identity is
    /// readable again.
    #[must_use = "a delta read is settled by a `drain_round` call, or by the chain"]
    pub fn read_storage_handle(&self, address: Address, index: U256) -> DeltaRead<'_> {
        let key = (address, index);
        let origin = {
            let state = self.state.lock();
            match state.resolved(&key, self.anchor.number, self.retention) {
                Some(value) => DeltaReadOrigin::Chain(value),
                None => DeltaReadOrigin::Hub(self.reads.read_storage(address, index)),
            }
        };
        DeltaRead {
            hub: self,
            key,
            origin,
        }
    }

    /// Read one storage word at this generation's anchor — the sequential
    /// caller's convenience over [`Self::read_storage_handle`]: register the
    /// read, drain one round (the read it just queued, plus anything else
    /// already waiting), and await the answer.
    ///
    /// A chain hit skips the drain entirely. A caller that wants its reads
    /// coalesced across several fibers registers them all and drains once — see
    /// [`Self::read_storage_handle`] and [`Self::drain_round`].
    ///
    /// # Errors
    ///
    /// The round could not answer the read: the member was refused
    /// ([`ReadHubError::Rpc`]), or the round was abandoned before it settled
    /// ([`ReadHubError::Abandoned`]). Nothing was settled, so a later read
    /// retries.
    pub async fn read_storage(
        &self,
        address: Address,
        index: U256,
    ) -> Result<StorageValue, ReadHubError> {
        let read = self.read_storage_handle(address, index);
        if read.is_chain_hit() {
            return read.await;
        }
        self.reads.drain_round().await;
        read.await
    }

    /// Read one account at this generation's anchor: the generation's [`ReadHub`]
    /// answers it (balance, nonce, and code in one batch member group), coalesced
    /// with every other read at the anchor. Accounts are never part of the delta
    /// — only settled storage words cross generations — so this is the hub's own
    /// seam, not a second one.
    pub fn read_account(&self, address: Address) -> ReadFuture<AccountRead> {
        self.reads.read_account(address)
    }

    /// Record one resolved candidate read's use at this generation's block.
    fn touch(&self, key: Slot) {
        self.state.lock().touch(key, self.anchor.number);
    }

    /// Settle one hub answer into this generation's own map, stamping the use
    /// that came with it. Called once per resolving read handle, so a value two
    /// handles coalesced onto settles on each handle's poll — the same value, at
    /// the same generation, with the same stamp.
    fn settle(&self, key: Slot, value: StorageValue) {
        self.state.lock().settle(key, value, self.anchor.number);
    }

    /// Seal this generation's own map into an immutable node over a base
    /// compacted to the threshold, and return it — the link a child walks.
    ///
    /// The move is a take, never a copy: whatever this generation settles
    /// afterwards lands in its late map and stays invisible to every child,
    /// the exact visibility boundary of the control's clone-at-transition.
    /// Under retention the fold drops what the policy has expired at THIS
    /// generation — the oldest generation that can still see the folded root.
    fn seal(
        &self,
        max_depth: u32,
        retention: Retention,
    ) -> (Arc<DeltaSnapshot>, Option<CompactionReport>) {
        let mut state = self.state.lock();

        if state.sealed {
            return (Arc::clone(&state.node), None);
        }

        let (base, compaction) = compact_base(
            Arc::clone(&state.node),
            max_depth,
            self.anchor.number,
            retention,
        );

        let delta = std::mem::take(&mut state.values);

        let used = std::mem::take(&mut state.used);

        // The count is recomputed against the folded base: the fold may have
        // dropped an expired record this generation re-settled or re-seeded,
        // and that identity now lives only in `delta`.
        let new_keys = delta.keys().filter(|key| !base.holds(key)).count();

        state.new_keys = 0;

        let parent = if base.is_void() { None } else { Some(base) };

        let sealed = Arc::new(DeltaSnapshot::node(parent, delta, used, new_keys));

        state.node = Arc::clone(&sealed);

        state.sealed = true;

        (sealed, compaction)
    }
}

/// The synchronous storage-view seam a `BotStateDb`-shaped consumer reads
/// through (see [`StorageView`]): a chain hit at THIS generation's anchor, and
/// a place to settle the caller's fallback answer.
///
/// The read is a chain hit — the answer, and the recency stamp, a candidate
/// read of a served entry gets — with no round: an identity the chain cannot
/// answer (unknown, or expired under retention) is the CALLER's fallback to
/// take, and settles back through [`StorageView::put`]. The async round
/// machinery stays the scheduler's seam ([`DeltaHub::read_storage_handle`]),
/// never a synchronous wrapper's.
impl StorageView for DeltaHub {
    fn get(&self, address: Address, index: U256) -> Option<StorageValue> {
        let key = (address, index);
        let mut state = self.state.lock();
        let value = state.resolved(&key, self.anchor.number, self.retention)?;
        state.touch(key, self.anchor.number);
        Some(value)
    }

    /// Settle one fallback answer into this generation: it joins the
    /// generation's own values (shadowing every ancestor, and travelling into
    /// the next generation at seal) and stamps the use that came with it —
    /// the same settle a resolved candidate read performs. A settle that lands
    /// after a child sealed this generation stays in this handle's late map,
    /// visible to this handle, to no ancestor and to no child.
    fn put(&self, address: Address, index: U256, value: StorageValue) {
        self.settle((address, index), value);
    }
}

/// Which side of the read seam answered one candidate storage read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOrigin {
    /// The generation's chain held the entry (its own delta, a seeded write, or
    /// an ancestor's settled word): answered with no RPC and nothing queued.
    Snapshot,
    /// The chain could not answer — unknown, or expired under retention — so the
    /// read registered with the generation's [`ReadHub`] and a round answers it.
    Hub,
}

/// One candidate storage read at a generation: a chain hit's value, or a hub
/// round's pending answer.
///
/// Polling resolves the chain hit (stamping its recency on the spot) or the
/// hub's answer (settling it into this generation's own map, then stamping
/// recency). A failed or abandoned round settles nothing.
#[must_use = "a read is settled by a `drain_round` call, or by the chain"]
pub struct DeltaRead<'h> {
    hub: &'h DeltaHub,
    key: Slot,
    origin: DeltaReadOrigin,
}

/// The read's answer source: a chain value, or the hub's channel.
enum DeltaReadOrigin {
    /// The chain answered at registration.
    Chain(StorageValue),
    /// The generation's hub owns the identity; its round answers this handle.
    Hub(ReadFuture<StorageValue>),
}

impl DeltaRead<'_> {
    /// Whether the chain answered this read without any round.
    #[must_use]
    pub const fn is_chain_hit(&self) -> bool {
        matches!(self.origin, DeltaReadOrigin::Chain(_))
    }

    /// Which side of the seam owns this read's answer.
    #[must_use]
    pub const fn origin(&self) -> ReadOrigin {
        match self.origin {
            DeltaReadOrigin::Chain(_) => ReadOrigin::Snapshot,
            DeltaReadOrigin::Hub(_) => ReadOrigin::Hub,
        }
    }
}

impl Future for DeltaRead<'_> {
    type Output = Result<StorageValue, ReadHubError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match &mut this.origin {
            DeltaReadOrigin::Chain(value) => {
                let value = *value;
                this.hub.touch(this.key);
                Poll::Ready(Ok(value))
            }
            DeltaReadOrigin::Hub(read) => match Pin::new(read).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(value)) => {
                    this.hub.settle(this.key, value);
                    Poll::Ready(Ok(value))
                }
                // A refused member and an abandoned round leave the generation
                // untouched: the identity stays readable and a later read
                // retries.
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            },
        }
    }
}

/// Compact `base` when linking one more generation would push past
/// `max_depth`, so every node stays within the documented walk bound.
fn compact_base(
    base: Arc<DeltaSnapshot>,

    max_depth: u32,

    now: u64,

    retention: Retention,
) -> (Arc<DeltaSnapshot>, Option<CompactionReport>) {
    if base.depth().saturating_add(1) > max_depth {
        let (root, report) = base.compact(now, retention);

        (root, Some(report))
    } else {
        (base, None)
    }
}

/// What one parent-linked delta-chain advance did — the control's
/// [`CarryReport`](super::read_hub::CarryReport) shape with the clone cost
/// replaced by the link cost and the compaction cost reported apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaReport {
    /// The transition that happened.
    pub outcome: CarryOutcome,

    /// The identities of the child's final writes APPLIED to the snapshot
    /// `(address, slot)`, in BAL order.
    pub writes: Vec<(Address, U256)>,

    /// How many of `writes` replaced a value the snapshot still served — the
    /// known-key overwrite path, whatever `relevance` says. Under retention a
    /// write to an expired entry takes the seeding path instead (module docs).
    pub writes_overwritten: usize,

    /// Final writes left UNKNOWN: address outside the relevance set.
    pub writes_skipped: usize,

    /// Wall time of the child BAL's fetch and full decode.
    pub bal_latency: Duration,

    /// Wall time of the parent link (sealing the parent generation and
    /// building the child's delta), compaction cost excluded.
    pub link_time: Duration,

    /// The compaction fold this transition paid, when one was due.
    pub compaction: Option<CompactionReport>,

    /// The identities the installed generation HOLDS after the transition:
    /// every value in its chain, including entries retention has already
    /// expired but no fold has yet dropped. An identity count, never bytes —
    /// one entry is a value plus its recency record, not a measured footprint.
    pub held: usize,

    /// The identities the installed generation still serves at its own anchor
    /// after the transition — `held` minus the entries idle retention has
    /// expired. `held - live` is what a fold will drop, so held memory shrinks
    /// at compaction rather than at the instant an entry expires.
    pub live: usize,
}

impl DeltaReport {
    /// The no-op report of a same-anchor repeat: no BAL work, no link, and the
    /// installed generation's sizes unchanged (reported all the same, so every
    /// row of a transition carries them).
    fn already_current(bal_latency: Duration, hub: &DeltaHub) -> Self {
        Self {
            outcome: CarryOutcome::AlreadyCurrent,

            writes: Vec::new(),

            writes_overwritten: 0,

            writes_skipped: 0,

            bal_latency,

            link_time: Duration::ZERO,

            compaction: None,

            held: hub.snapshot_len(),

            live: hub.live_len(),
        }
    }
}

/// The session-scoped owner of the per-generation [`DeltaHub`] — the
/// delta-chain counterpart of the control's `ReadHubCache`.
pub struct DeltaSnapshotCache {
    provider: AlloyProvider,

    max_depth: u32,

    retention: Retention,

    current: Mutex<Option<(ReadAnchor, Arc<DeltaHub>)>>,
}

impl DeltaSnapshotCache {
    /// An empty owner at [`DEFAULT_MAX_DEPTH`] and [`Retention::Unbounded`].
    #[must_use]
    pub fn new(provider: &AlloyProvider) -> Self {
        Self::with_max_depth(provider, DEFAULT_MAX_DEPTH)
    }

    /// An empty owner whose compaction threshold bounds every node's
    /// [`depth`](DeltaSnapshot::depth), under [`Retention::Unbounded`]. A
    /// threshold below 1 is treated as 1: every generation then keeps at most
    /// its own delta over one compacted root.
    #[must_use]
    pub fn with_max_depth(provider: &AlloyProvider, max_depth: u32) -> Self {
        Self::with_max_depth_and_retention(provider, max_depth, Retention::Unbounded)
    }

    /// An empty owner at [`DEFAULT_MAX_DEPTH`] under `retention`.
    #[must_use]
    pub fn with_retention(provider: &AlloyProvider, retention: Retention) -> Self {
        Self::with_max_depth_and_retention(provider, DEFAULT_MAX_DEPTH, retention)
    }

    /// An empty owner whose compaction threshold bounds every node's
    /// [`depth`](DeltaSnapshot::depth) and whose retention policy decides what
    /// stays served (module docs). A threshold below 1 is treated as 1.
    #[must_use]
    pub fn with_max_depth_and_retention(
        provider: &AlloyProvider,
        max_depth: u32,
        retention: Retention,
    ) -> Self {
        Self {
            provider: provider.clone(),

            max_depth: max_depth.max(1),

            retention,

            current: Mutex::new(None),
        }
    }

    /// The generation for `anchor`: the installed one when the whole anchor
    /// (number AND hash) is unchanged, otherwise a fresh cold generation that
    /// becomes the installed one — the control's `for_anchor` contract, where
    /// a replacement drops the retired generation's values with it.
    pub fn for_anchor(&self, anchor: ReadAnchor) -> Arc<DeltaHub> {
        let mut current = self.current.lock();

        if let Some((installed, hub)) = current.as_ref() {
            if *installed == anchor {
                return Arc::clone(hub);
            }
        }

        let hub = Arc::new(DeltaHub::fresh(&self.provider, anchor, self.retention));

        *current = Some((anchor, Arc::clone(&hub)));

        hub
    }

    /// The opt-in parent-linked carry-forward over the delta chain: install
    /// `child` as the current generation linking the settled parent chain,
    /// with EVERY final write of the child's exact-hash canonical block access
    /// list applied to the child's own delta.
    ///
    /// The transition is the control's `advance_bal`, item for item: the link
    /// is exact (the installed anchor exactly one block below the child with
    /// the child's actual parent hash), the child BAL is fetched and fully
    /// decoded before anything links (a skip yields a cold child that serves
    /// no parent values), the write policy is the control's (update served
    /// keys, seed only relevant new ones — under retention an expired entry is
    /// new), and a same-anchor call is idempotent and repeats no BAL work.
    pub async fn advance_bal(
        &self,

        child: ReadAnchor,

        child_parent_hash: B256,

        relevance: &[Address],
    ) -> (Arc<DeltaHub>, DeltaReport) {
        // Idempotence precedes any RPC: the same child anchor repeats no BAL
        // work, and no lock guard crosses the fetch.
        if let Some(hub) = self.installed(child) {
            let report = DeltaReport::already_current(Duration::ZERO, &hub);
            return (hub, report);
        }

        let prepared = PreparedBal::prepare(&self.provider, child).await;

        self.apply_prepared(
            child,
            child_parent_hash,
            &RelevanceSet::from_slice(relevance),
            &prepared,
        )
    }

    /// The generation installed at exactly `anchor`, when there is one.
    fn installed(&self, anchor: ReadAnchor) -> Option<Arc<DeltaHub>> {
        let current = self.current.lock();

        current
            .as_ref()
            .filter(|(installed, _)| *installed == anchor)
            .map(|(_, hub)| Arc::clone(hub))
    }

    /// Apply a prepared child BAL to `child` — the transition
    /// [`Self::advance_bal`] performs with its fetch and decode already done,
    /// item for item: the same exact lineage classification, the same sealed
    /// parent chain linked only when the lineage matched and the list decoded,
    /// the same write policy and the same relevance MEMBERSHIP
    /// ([`RelevanceSet`]: one hash probe per write), the same
    /// retention/compaction transition, and the same idempotent same-anchor
    /// report.
    ///
    /// `prepared` must have been prepared at `child`'s own anchor: the
    /// [`BalHeadDriver`] enforces that exact-hash fence before calling here. The
    /// report's `bal_latency` is the prepared phase's own wall time, so a
    /// lifecycle report and a synchronous advance report are comparable field
    /// for field.
    fn apply_prepared(
        &self,
        child: ReadAnchor,
        child_parent_hash: B256,
        relevance: &RelevanceSet,
        prepared: &PreparedBal,
    ) -> (Arc<DeltaHub>, DeltaReport) {
        let final_writes = prepared.final_writes();

        let bal = prepared.skip().cloned();

        let bal_latency = prepared.latency();

        // Link, seal, and install under one lock hold, so the lineage check
        // and the anchor move are one transition — and the link seals the same
        // installed parent the check validated.
        let mut current = self.current.lock();

        if let Some((installed, hub)) = current.as_ref() {
            if *installed == child {
                let report = DeltaReport::already_current(bal_latency, hub);
                return (Arc::clone(hub), report);
            }
        }

        let lineage = classify_link(
            current.as_ref().map(|(anchor, _)| anchor),
            child,
            child_parent_hash,
        );

        let linked = lineage.is_none() && bal.is_none();

        let mut carried_values = 0;

        let mut link_time = Duration::ZERO;

        let mut compaction = None;

        let mut base = None;

        if linked {
            if let Some((_, parent_hub)) = current.as_ref() {
                let link_started = Instant::now();

                let (sealed, folded) = parent_hub.seal(self.max_depth, self.retention);

                let seal_elapsed = link_started.elapsed();

                carried_values = sealed.entries();

                link_time = seal_elapsed.saturating_sub(
                    folded
                        .as_ref()
                        .map_or(Duration::ZERO, |report| report.elapsed),
                );

                compaction = folded;

                base = Some(sealed);
            }
        }

        let base = base.unwrap_or_else(|| Arc::new(DeltaSnapshot::root()));

        let (values, used, applied) =
            seed_final_writes(final_writes, relevance, &base, child.number, self.retention);

        let hub = Arc::new(DeltaHub::generation(
            &self.provider,
            child,
            self.retention,
            GenState {
                values,

                used,

                node: base,

                sealed: false,

                new_keys: applied.new_keys,
            },
        ));

        let outcome = if linked {
            CarryOutcome::Carried { carried_values }
        } else {
            CarryOutcome::Cold { lineage, bal }
        };

        // The installed generation's sizes, read before the handle is handed back:
        // an identity count of what the chain holds and what it still serves.
        let (held, live) = (hub.snapshot_len(), hub.live_len());

        *current = Some((child, Arc::clone(&hub)));

        (
            hub,
            DeltaReport {
                outcome,

                writes: applied.applied,

                writes_overwritten: applied.overwritten,

                writes_skipped: applied.skipped,

                bal_latency,

                link_time,

                compaction,

                held,

                live,
            },
        )
    }

    /// The installed anchor (`Option::None` before the first `for_anchor`).
    #[must_use]
    pub fn anchor(&self) -> Option<ReadAnchor> {
        self.current.lock().as_ref().map(|(anchor, _)| *anchor)
    }
}

// ── The asynchronous canonical-head lifecycle ──────────────────────────────
//
// A canonical head arrives from outside (a block subscription, a poller, a
// replay), and the BAL that makes the head's generation cheap to read is
// fetched and decoded separately, possibly after candidates are already running
// at that head. [`BalHeadDriver`] is the seam between those two clocks: it
// records the delivered head by its exact hash, hands every caller at that head
// a cold generation until the prepared list applies, and refuses any apply that
// does not name the head the driver currently holds.

/// The ticket one [`BalHeadDriver::deliver`] minted: the head identity a
/// preparation is applied against, and the delivery sequence that says whether
/// that head is still the driver's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadTicket {
    anchor: ReadAnchor,
    seq: u64,
}

impl HeadTicket {
    /// The exact-hash head this ticket names: readiness is keyed by this whole
    /// anchor, never by the number alone.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }
}

/// Why a caller at one exact head got a cold generation instead of a settled
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCold {
    /// The head is delivered and its prepared BAL has not been applied yet:
    /// parent values are withheld rather than served at the wrong anchor. The
    /// synchronous view's answer for an unsettled head; a caller that wants
    /// the settled answer waits for it ([`BalHeadDriver::wait_for_head`]),
    /// which joins a genuinely live preparation and resolves terminal — a
    /// preparation that is gone or was never claimed answers
    /// [`Self::Abandoned`].
    Unprepared,
    /// A later canonical head superseded this one before it settled. The
    /// abandoned branch is never resurrected.
    Superseded,
    /// No head with this exact anchor was ever delivered to the driver.
    Undelivered,
    /// The head's preparation ended without applying — a dropped (cancelled)
    /// claim — or no preparation ever claimed this delivered head. The wait's
    /// terminal answer at this exact anchor: the caller takes its own fallback
    /// lane and nothing is left parked. A later explicit retry may still
    /// settle the head; NEW callers then read the settled generation, while a
    /// wait that already resolved is never retroactively changed.
    Abandoned,
    /// The head SETTLED at its exact hash and serves no parent value, for this
    /// reason.
    Settled {
        /// Why the settled parent chain was not carried; `None` when the lineage
        /// matched and the list is what stepped aside.
        lineage: Option<CarryRefusal>,
        /// Why the head's list was not applied; `None` when it decoded.
        bal: Option<BalSkip>,
    },
}

impl HeadCold {
    /// The closed-set label this cold reason reports — the one vocabulary a
    /// caller's diagnostic line and its metric labels share.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unprepared => "unprepared",
            Self::Superseded => "superseded",
            Self::Undelivered => "undelivered",
            Self::Abandoned => "abandoned",
            Self::Settled { lineage, bal } => {
                if lineage.is_some() {
                    "lineage-refused"
                } else if bal.is_some() {
                    "bal-unreadable"
                } else {
                    "settled"
                }
            }
        }
    }
}

/// Whether a caller's snapshot at one exact head is that head's settled
/// generation, or the cold path with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadReadiness {
    /// The head settled: reads at the view's hub resolve from its own
    /// generation, which carries the parent chain when the lineage matched.
    Ready,
    /// The cold path: the view's hub serves no parent value, and `reason` says
    /// why.
    Cold(HeadCold),
}

impl HeadReadiness {
    /// Whether the head settled.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Why the caller got a cold generation, when it did.
    #[must_use]
    pub const fn cold_reason(&self) -> Option<&HeadCold> {
        match self {
            Self::Ready => None,
            Self::Cold(reason) => Some(reason),
        }
    }
}

/// The snapshot one exact head answers with: the generation to read, and
/// whether that generation is the head's settled one.
pub struct HeadView {
    /// The generation to read. A settled head's own generation when
    /// [`Self::readiness`] is [`HeadReadiness::Ready`]; otherwise a cold
    /// generation pinned at the same anchor that carries nothing.
    pub hub: Arc<DeltaHub>,
    /// Whether the head settled, or why the cold path answered.
    pub readiness: HeadReadiness,
}

/// What one apply — or one whole background advance — did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadOutcome {
    /// The prepared list applied to the head it was prepared for: the
    /// generation is installed and later reads at that exact hash are ready.
    Settled(Arc<DeltaReport>),
    /// The head was already settled: nothing was installed and nothing moved.
    AlreadySettled,
    /// Another caller already owns this head's preparation: this call fetched
    /// nothing and waited for nothing.
    InFlight,
    /// The ticket names a head a later delivery superseded, so nothing was
    /// installed.
    Superseded {
        /// The head the driver holds now.
        head: ReadAnchor,
    },
    /// The prepared list was fetched at a different anchor than the ticket's
    /// head: nothing was installed, whatever the list says.
    Foreign {
        /// The anchor the list was prepared at.
        prepared: ReadAnchor,
        /// The anchor the ticket names.
        head: ReadAnchor,
    },
}

/// One preparation CLAIM ATTEMPT's terminal result: what the waiters that
/// joined that attempt are woken with.
///
/// Completion is scoped to the attempt, not to the head, because a head can be
/// claimed more than once: a dropped (cancelled) advance releases its claim and
/// a retry prepares the same anchor again. An attempt reaches exactly one of
/// these, and reaching one is what ends every wait registered on it
/// ([`BalHeadDriver::wait_for_head`]): the head settled, a later delivery
/// replaced it, or the preparation that owned the attempt ended without
/// applying. The first two re-read the exact anchor for the settled truth;
/// `Abandoned` is itself the wait's terminal answer — the attempt has no live
/// preparation left to settle it — so its waiters resolve
/// [`HeadCold::Abandoned`] instead of registering for one. Reaching one is
/// final for that attempt — first terminal wins, for every variant — so a
/// transition that arrives afterwards (the delivery that supersedes the head an
/// abandoned attempt held included) never rewrites what the attempt already
/// published. A retry publishes on its own private epoch, so it settles the head
/// for NEW callers and for its own waiters while the abandonment an earlier
/// attempt published stays that attempt's waiters' answer: a wait that already
/// resolved is never retroactively changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadCompletion {
    /// The head's own list applied to its exact anchor — carried or cold. The
    /// re-read then answers [`HeadReadiness::Ready`], or cold with the exact
    /// `Settled` reason.
    Settled,
    /// A later delivery replaced this head before it settled. The abandoned
    /// branch is never resurrected and never served.
    Superseded,
    /// The preparation that owned this head ended without applying (a
    /// cancelled advance, or a list prepared at a foreign anchor): the
    /// generation's waiters resolve [`HeadCold::Abandoned`] and take their
    /// fallback lane.
    Abandoned,
}

/// The head driver's cumulative counters — the lifecycle's own profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeadStats {
    /// Heads delivered (an idempotent repeat is not a delivery).
    pub delivered: u64,
    /// Deliveries that repeated the current head: no ticket minted, no work.
    pub repeated: u64,
    /// Delivered heads a later delivery superseded before they settled.
    pub superseded: u64,
    /// Heads this driver settled, carried or cold.
    pub settled: u64,
    /// Deliveries of a head whose anchor was already installed — a reorg back
    /// to the installed block — settled on the spot with no BAL work.
    pub already_current: u64,
    /// Apply attempts discarded: a superseded ticket or a foreign list.
    pub discarded: u64,
    /// Callers served a settled head's generation.
    pub ready_views: u64,
    /// Callers served a cold generation.
    pub cold_views: u64,
}

/// One preparation CLAIM ATTEMPT on one head — the private identity a wait is
/// registered against.
///
/// A head is delivered once but can be claimed more than once: a dropped
/// (cancelled) advance releases its claim and a later caller prepares the same
/// anchor again. Completion is therefore scoped to the attempt, not the head: a
/// fresh attempt mints its own epoch and its own completion channel, so a retry
/// publishes on ITS channel and can never rewrite an abandonment an earlier
/// attempt's waiters have not polled yet. Neither the epoch nor the channel is
/// public — the ticket's `seq` is the only sequencing a caller sees, and a retry
/// reuses it.
struct ClaimAttempt {
    /// Distinguishes same-anchor attempts. `0` is the attempt a delivery itself
    /// owns, which no caller has taken; every taken attempt mints a larger
    /// epoch, and the epoch is what keeps a released attempt's publication off
    /// a later attempt's channel.
    epoch: u64,
    /// This attempt's terminal result, `None` until the attempt ends: the one
    /// channel every waiter that joined THIS attempt is woken on.
    completion: watch::Sender<Option<HeadCompletion>>,
}

impl ClaimAttempt {
    /// The attempt a delivery owns: no caller has taken it and nothing is
    /// published.
    fn delivered() -> Self {
        Self::undecided(0)
    }

    /// A fresh attempt at `epoch`: its own channel, so the attempt's terminal
    /// result is its own.
    fn minted(epoch: u64) -> Self {
        Self::undecided(epoch)
    }

    /// The attempt a head's settled truth is published on when the attempt that
    /// owned the head already ended: its terminal result is published at
    /// creation, because nothing is left to prepare.
    fn settled(epoch: u64) -> Self {
        let (completion, _) = watch::channel(Some(HeadCompletion::Settled));
        Self { epoch, completion }
    }

    /// An attempt nothing has decided yet — the shape a delivery and a fresh
    /// claim attempt share.
    fn undecided(epoch: u64) -> Self {
        let (completion, _) = watch::channel(None);
        Self { epoch, completion }
    }

    /// The terminal result this attempt published, if it has one.
    fn published(&self) -> Option<HeadCompletion> {
        *self.completion.borrow()
    }

    /// Publish this attempt's terminal result — FIRST TERMINAL WINS, for every
    /// variant.
    ///
    /// One attempt reaches exactly one terminal answer, and the first one
    /// published is what the waiters that joined the attempt are woken with. A
    /// later publication for the SAME attempt is dropped, whichever variant it
    /// carries: a settle is not rewritten by the claim's release, a released
    /// attempt's abandonment is not rewritten by the delivery that supersedes
    /// the head it held, and a supersession is not rewritten by the release
    /// that follows it. Nothing needs to republish over its own attempt — a
    /// retry mints a new attempt with its own epoch and channel and publishes
    /// there, so a later attempt's real terminal never depends on overwriting
    /// an earlier attempt's.
    fn publish(&self, completion: HeadCompletion) {
        if self.completion.borrow().is_none() {
            self.completion.send_replace(Some(completion));
        }
    }

    /// This attempt's completion channel, for a waiter joining a genuinely live
    /// preparation.
    fn subscribe(&self) -> watch::Receiver<Option<HeadCompletion>> {
        self.completion.subscribe()
    }
}

/// The delivery identity every pose of the current head carries: the exact-hash
/// anchor, the parent hash its lineage is verified against, and the public
/// delivery sequence its ticket names.
struct HeadDelivery {
    anchor: ReadAnchor,
    parent_hash: B256,
    seq: u64,
}

/// A delivered head that has not settled: the ONE cold generation every caller
/// at it shares until its BAL applies, and the claim attempt that owns — or,
/// once released, owned — its preparation.
struct UnsettledHead {
    delivery: HeadDelivery,
    /// The cold generation callers read while the head is unprepared, dropped at
    /// settle so the settled generation replaces it wholesale.
    provisional: Arc<DeltaHub>,
    attempt: ClaimAttempt,
}

/// A settled head: from here the anchor's generation is the cache's, so the cold
/// generation is gone. `report` is the transition this driver ran, absent when a
/// delivery found the generation already installed.
struct SettledHead {
    delivery: HeadDelivery,
    report: Option<Arc<DeltaReport>>,
    attempt: ClaimAttempt,
}

/// The head the driver holds, in exactly one pose.
///
/// The pose is what the payload combinations make type-safe: an unsettled head
/// always carries the cold generation its callers share and a settled one never
/// does, and "a caller owns the preparation" is a pose rather than a flag — so
/// the liveness a wait registers on and the data the state carries cannot
/// disagree.
enum CurrentHead {
    /// Delivered with nothing owning its preparation: a direct `prepare` +
    /// `apply` moves it, and a released (cancelled) attempt leaves it here with
    /// that attempt's own terminal result already published.
    Unclaimed(UnsettledHead),
    /// A live claim attempt owns the preparation: the state a wait registers on
    /// and a second concurrent advance joins as `InFlight`.
    Preparing(UnsettledHead),
    /// The head settled, carried or cold.
    Settled(SettledHead),
}

impl CurrentHead {
    fn delivery(&self) -> &HeadDelivery {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => &head.delivery,
            Self::Settled(head) => &head.delivery,
        }
    }

    fn anchor(&self) -> ReadAnchor {
        self.delivery().anchor
    }

    fn attempt(&self) -> &ClaimAttempt {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => &head.attempt,
            Self::Settled(head) => &head.attempt,
        }
    }

    /// The cold generation the head shares with its callers, while it has one.
    fn provisional(&self) -> Option<&Arc<DeltaHub>> {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => Some(&head.provisional),
            Self::Settled(_) => None,
        }
    }

    /// The report this driver's apply produced, once the head settled.
    fn report(&self) -> Option<Arc<DeltaReport>> {
        match self {
            Self::Settled(head) => head.report.as_ref().map(Arc::clone),
            Self::Unclaimed(_) | Self::Preparing(_) => None,
        }
    }

    /// Whether the head's current attempt already published its own end, so a
    /// settle must mint a fresh private epoch instead of rewriting it.
    fn published_abandonment(&self) -> bool {
        matches!(
            self,
            Self::Unclaimed(head)
                if matches!(head.attempt.published(), Some(HeadCompletion::Abandoned))
        )
    }

    /// Take the live claim `epoch` on an unclaimed head: the pose a caller owns
    /// the preparation in.
    fn claimed(self, epoch: u64) -> Self {
        match self {
            Self::Unclaimed(head) => Self::Preparing(UnsettledHead {
                attempt: ClaimAttempt::minted(epoch),
                ..head
            }),
            other => other,
        }
    }

    /// Release the live claim attempt `epoch` on the head `seq` delivered —
    /// publishing its own end — and leave the pose unclaimed, so a later caller
    /// prepares the same head normally. Any other pose is untouched: a settle or
    /// a supersession already decided that attempt's waiters' answer, and a
    /// release never rewrites it.
    fn released(self, seq: u64, epoch: u64) -> Self {
        match self {
            Self::Preparing(head) if head.delivery.seq == seq && head.attempt.epoch == epoch => {
                head.attempt.publish(HeadCompletion::Abandoned);
                Self::Unclaimed(head)
            }
            other => other,
        }
    }

    /// Publish the settle on this pose's attempt: the live attempt's own channel,
    /// the delivery's undecided one, or — when the attempt that held the head
    /// already published its own end — the `fresh` private epoch.
    fn publish_settle(&mut self, fresh: Option<u64>) {
        match self {
            Self::Unclaimed(head) => match fresh {
                Some(epoch) => head.attempt = ClaimAttempt::settled(epoch),
                None => head.attempt.publish(HeadCompletion::Settled),
            },
            Self::Preparing(head) => head.attempt.publish(HeadCompletion::Settled),
            Self::Settled(head) => head.attempt.publish(HeadCompletion::Settled),
        }
    }

    /// The settled pose, holding the report of the transition that settled it.
    ///
    /// A settle ALWAYS stores the report it just produced: the settled pose never
    /// keeps an older transition's report, whichever pose the settle found. `None`
    /// belongs to the delivery that found its generation already installed — it
    /// runs no transition, so it passes through no settle and can never overwrite
    /// the report of one that did.
    fn settled(self, report: Arc<DeltaReport>) -> Self {
        match self {
            Self::Settled(head) => Self::Settled(SettledHead {
                report: Some(report),
                ..head
            }),
            Self::Unclaimed(head) | Self::Preparing(head) => Self::Settled(SettledHead {
                delivery: head.delivery,
                report: Some(report),
                attempt: head.attempt,
            }),
        }
    }
}

/// The installed generation: the anchor the cache has installed and the readiness
/// its OWN settle derived. The cache owns the hub; the lifecycle owns why it
/// carries — one settle-derived readiness, never re-derived from a hub.
struct InstalledGeneration {
    anchor: ReadAnchor,
    readiness: HeadReadiness,
}

/// The one previous anchor a lagging caller can still read: the generation the
/// installing transition replaced, with the handle that was installed there and
/// the readiness it reported.
///
/// Set only by a transition that linked (the new head was exactly the previous
/// anchor + 1 with a matching parent hash), and cleared by any settle that did
/// not, so a gap makes the window smaller, never wrong. This is a window, not
/// retention: the handle is the generation the seal already folded into the new
/// head's node, so it costs one `Arc` and an empty handle-local map — not one
/// snapshot.
struct PreviousGeneration {
    anchor: ReadAnchor,
    hub: Arc<DeltaHub>,
    readiness: HeadReadiness,
}

/// What one snapshot at an anchor reads, decided by the lifecycle under its lock.
enum Lookup {
    /// The anchor is the installed generation: the hub is the cache's, and the
    /// readiness is the one that generation's settle derived.
    Installed { readiness: HeadReadiness },
    /// The driver's current head settled but its anchor is not the installed
    /// generation: the generation is still the cache's, and the readiness is the
    /// one the head's settle derived.
    Settled { readiness: HeadReadiness },
    /// The current head is unprepared: every caller at it shares this one cold
    /// generation.
    Unprepared { hub: Arc<DeltaHub> },
    /// The anchor outside the installed head that the window still covers.
    Previous {
        hub: Arc<DeltaHub>,
        readiness: HeadReadiness,
    },
    /// Nothing the lifecycle knows answers at this anchor.
    Cold { reason: HeadCold },
}

/// What one claim attempt's prologue decided.
enum ClaimStart {
    /// The head is this caller's to prepare now, on this private epoch.
    Claimed(u64),
    /// Nothing was claimed; this is the answer the advance returns.
    Refused(HeadOutcome),
}

/// What one apply decided before the cache is touched.
enum ApplyPlan {
    /// Nothing is installed; this is the answer the apply returns.
    Refused(HeadOutcome),
    /// Install `anchor`'s prepared list over the verified parent lineage.
    Admitted {
        anchor: ReadAnchor,
        parent_hash: B256,
        /// The installed anchor the transition seals over, when there is one.
        prior: Option<ReadAnchor>,
    },
}

/// The BAL head LIFECYCLE: the driver's one authority over a stream of canonical
/// heads.
///
/// # The three poses
///
/// The head the driver holds — `current` — is in exactly one of
/// [`CurrentHead::Unclaimed`], [`CurrentHead::Preparing`] or
/// [`CurrentHead::Settled`]. Unclaimed and Preparing carry the delivery identity,
/// the one cold generation their callers share and the claim attempt a wait
/// registers against; Settled carries the delivery identity, the report of the
/// transition that settled it and the attempt its waiters were woken on. A
/// cancelled preparation returns the head to Unclaimed — the same head, not a
/// new pose — and a spawned phase's release stays a spawned-phase concept, never
/// a driver pose.
///
/// # The projections
///
/// Everything else a caller can observe is an orthogonal projection of that pose
/// plus the generation history: [`InstalledGeneration`] (the readiness the
/// installed head's own settle derived), [`PreviousGeneration`] (the one anchor
/// the read window still covers), the superseded anchor (the one abandoned head a
/// lagging caller learns about), the per-attempt completion, the public ticket
/// sequence and the counters. Cold is a readiness value, abandonment is one
/// attempt's terminal result, and none of them is a second lifecycle authority:
/// the cache installs hubs, this machine is the only lifecycle writer.
#[derive(Default)]
struct HeadLifecycle {
    current: Option<CurrentHead>,
    /// The anchor the last delivery superseded: one slot, so a caller still
    /// working at the abandoned head learns why its snapshot is cold.
    superseded: Option<ReadAnchor>,
    /// The installed generation's readiness, kept after its head is superseded so
    /// a caller still at that head gets the generation — and the truth about
    /// whether it carries — rather than a fresh cold one.
    installed: Option<InstalledGeneration>,
    previous: Option<PreviousGeneration>,
    seq: u64,
    /// The epoch the next preparation claim attempt mints. Private: public
    /// sequencing is the delivery `seq`, which a retry reuses.
    claim_epoch: u64,
    stats: HeadStats,
}

impl HeadLifecycle {
    /// The readiness the `anchor`'s own settle derived; a generation the machine
    /// holds no settle for is the ready one.
    fn readiness_of(&self, anchor: ReadAnchor) -> HeadReadiness {
        self.installed
            .as_ref()
            .filter(|generation| generation.anchor == anchor)
            .map_or(HeadReadiness::Ready, |generation| {
                generation.readiness.clone()
            })
    }

    /// Deliver one head: mint the public delivery sequence, retire the head it
    /// replaces, and put the new head in the pose its installation calls for.
    fn deliver(
        &mut self,
        anchor: ReadAnchor,
        parent_hash: B256,
        installed: Option<ReadAnchor>,
        cold: impl FnOnce() -> Arc<DeltaHub>,
    ) -> u64 {
        if let Some(seq) = self
            .current
            .as_ref()
            .map(CurrentHead::delivery)
            .filter(|held| held.anchor == anchor && held.parent_hash == parent_hash)
            .map(|held| held.seq)
        {
            self.stats.repeated = self.stats.repeated.saturating_add(1);
            return seq;
        }
        self.seq = self.seq.saturating_add(1);
        let seq = self.seq;
        if let Some(replaced) = self.current.take() {
            if replaced.provisional().is_some() {
                self.stats.superseded = self.stats.superseded.saturating_add(1);
            }
            // The replaced generation is terminal for its waiters: the wake is
            // what makes a stale registration resolve instead of sleeping through
            // its own abandonment. An attempt that already published its own end
            // keeps it — this supersession is the HEAD's transition, not a second
            // terminal for that attempt.
            replaced.attempt().publish(HeadCompletion::Superseded);
            self.superseded = Some(replaced.anchor());
        }
        self.stats.delivered = self.stats.delivered.saturating_add(1);
        let delivery = HeadDelivery {
            anchor,
            parent_hash,
            seq,
        };
        self.current = Some(if installed == Some(anchor) {
            self.stats.already_current = self.stats.already_current.saturating_add(1);
            // The readiness the installed generation's own settle derived — the
            // same record `lookup` reads — so a redelivery restates no transition
            // it did not perform.
            let readiness = self.readiness_of(anchor);
            self.installed = Some(InstalledGeneration { anchor, readiness });
            // A delivery of an installed block settles on the spot: its generation
            // is terminal the moment it exists.
            CurrentHead::Settled(SettledHead {
                delivery,
                report: None,
                attempt: ClaimAttempt::settled(0),
            })
        } else {
            CurrentHead::Unclaimed(UnsettledHead {
                delivery,
                provisional: cold(),
                attempt: ClaimAttempt::delivered(),
            })
        });
        seq
    }

    /// Take the live preparation claim on the ticket's head.
    ///
    /// Only one caller prepares a given head: a second concurrent advance on the
    /// same ticket is `InFlight`, an installed or settled head is
    /// `AlreadySettled`, and a ticket a later delivery superseded installs
    /// nothing. A fresh attempt on an unclaimed head mints its own private epoch
    /// and channel — its terminal result can never overwrite the one the attempt
    /// it replaces published — while the public ticket is untouched.
    fn claim(&mut self, anchor: ReadAnchor, seq: u64, installed: Option<ReadAnchor>) -> ClaimStart {
        let Some(current) = self.current.as_ref() else {
            return ClaimStart::Refused(HeadOutcome::Superseded { head: anchor });
        };
        let held = current.delivery();
        if held.seq != seq || held.anchor != anchor {
            return ClaimStart::Refused(HeadOutcome::Superseded { head: held.anchor });
        }
        if installed == Some(anchor) {
            return ClaimStart::Refused(HeadOutcome::AlreadySettled);
        }
        match current {
            CurrentHead::Preparing(_) => return ClaimStart::Refused(HeadOutcome::InFlight),
            // A settled head the cache no longer holds has nothing left to
            // prepare: the answer is the one an installed head gives.
            CurrentHead::Settled(_) => return ClaimStart::Refused(HeadOutcome::AlreadySettled),
            CurrentHead::Unclaimed(_) => {}
        }
        self.claim_epoch = self.claim_epoch.saturating_add(1);
        let epoch = self.claim_epoch;
        self.current = self.current.take().map(|current| current.claimed(epoch));
        ClaimStart::Claimed(epoch)
    }

    /// Release the live claim attempt a dropped preparation owned.
    fn release(&mut self, seq: u64, epoch: u64) {
        if let Some(head) = self.current.take() {
            self.current = Some(head.released(seq, epoch));
        }
    }

    /// Decide one apply under the lock: the exact-hash fence, the foreign-list
    /// fence, the installed head's idempotence, and which generation the
    /// transition seals over. A refusal is counted here and installs nothing.
    fn plan_apply(
        &mut self,
        anchor: ReadAnchor,
        seq: u64,
        prepared: ReadAnchor,
        installed: Option<ReadAnchor>,
    ) -> ApplyPlan {
        let Some(current) = self.current.as_ref() else {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Superseded { head: anchor });
        };
        let (held_anchor, held_seq, parent_hash) = {
            let held = current.delivery();
            (held.anchor, held.seq, held.parent_hash)
        };
        if held_seq != seq || held_anchor != anchor {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Superseded { head: held_anchor });
        }
        if prepared != held_anchor {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Foreign {
                prepared,
                head: held_anchor,
            });
        }
        if installed == Some(held_anchor) {
            // The head is already installed — this apply found it settled: its
            // generation is terminal and its waiters wake on the head's current
            // attempt.
            self.publish_settle();
            return ApplyPlan::Refused(HeadOutcome::AlreadySettled);
        }
        ApplyPlan::Admitted {
            anchor: held_anchor,
            parent_hash,
            prior: installed,
        }
    }

    /// Record one installed generation: the counters, the read window, and the
    /// readiness the transition derived from its own report.
    fn record_settle(
        &mut self,
        anchor: ReadAnchor,
        report: &Arc<DeltaReport>,
        prior: Option<(ReadAnchor, Arc<DeltaHub>)>,
    ) {
        self.stats.settled = self.stats.settled.saturating_add(1);
        // The two-anchor window: a linked transition makes the generation it just
        // sealed the previous anchor a lagging caller may read. A settle that did
        // not link (no parent, not consecutive, a mismatched parent hash, or an
        // unreadable list) leaves no previous slot, so the window shrinks across a
        // gap instead of pointing at a generation that never was this chain's
        // parent.
        match &report.outcome {
            CarryOutcome::Carried { .. } => {
                if let Some((prior_anchor, prior_hub)) = prior {
                    let readiness = self.readiness_of(prior_anchor);
                    self.previous = Some(PreviousGeneration {
                        anchor: prior_anchor,
                        hub: prior_hub,
                        readiness,
                    });
                }
            }
            CarryOutcome::Cold { .. } => self.previous = None,
            CarryOutcome::AlreadyCurrent => {}
        }
        self.installed = Some(InstalledGeneration {
            anchor,
            readiness: settled_readiness(Some(report.as_ref())),
        });
    }

    /// Publish the settle and move the current head to its settled pose — the
    /// cold generation dropped, the report and the attempt kept.
    ///
    /// A head can settle while its current attempt already published its own end:
    /// an out-of-band [`BalHeadDriver::apply`] on the head's ticket can land after
    /// the attempt that claimed the head was dropped, before any retry claims it.
    /// That attempt published `Abandoned`, and an attempt's terminal result is
    /// never rewritten, so [`Self::publish_settle`] mints a fresh private epoch
    /// and channel for the settle. Only the private epoch moves — the public
    /// ticket's `seq` is untouched, so no caller sees a new generation.
    fn settle_current(&mut self, report: Arc<DeltaReport>) {
        self.publish_settle();
        if let Some(current) = self.current.take() {
            self.current = Some(current.settled(report));
        }
    }

    /// Publish the head's settled truth on its current attempt without moving the
    /// pose (an apply that found the generation already installed).
    ///
    /// An ordinary live-claim settle, a delivery's undecided attempt, and a head
    /// that is already settled all publish on the attempt they already have:
    /// nothing was abandoned, so no new epoch is minted.
    fn publish_settle(&mut self) {
        let fresh = if self
            .current
            .as_ref()
            .is_some_and(CurrentHead::published_abandonment)
        {
            self.claim_epoch = self.claim_epoch.saturating_add(1);
            Some(self.claim_epoch)
        } else {
            None
        };
        if let Some(current) = self.current.as_mut() {
            current.publish_settle(fresh);
        }
    }

    /// What one snapshot at `anchor` reads, from the pose and the history alone.
    fn lookup(&self, anchor: ReadAnchor, installed: Option<ReadAnchor>) -> Lookup {
        if installed == Some(anchor) {
            return Lookup::Installed {
                readiness: self.readiness_of(anchor),
            };
        }
        match self.current.as_ref().filter(|head| head.anchor() == anchor) {
            Some(CurrentHead::Unclaimed(head) | CurrentHead::Preparing(head)) => {
                Lookup::Unprepared {
                    hub: Arc::clone(&head.provisional),
                }
            }
            Some(CurrentHead::Settled(_)) => Lookup::Settled {
                readiness: self.readiness_of(anchor),
            },
            None => match self
                .previous
                .as_ref()
                .filter(|previous| previous.anchor == anchor)
            {
                Some(previous) => Lookup::Previous {
                    hub: Arc::clone(&previous.hub),
                    readiness: previous.readiness.clone(),
                },
                None => Lookup::Cold {
                    reason: if self.superseded == Some(anchor) {
                        HeadCold::Superseded
                    } else {
                        HeadCold::Undelivered
                    },
                },
            },
        }
    }

    /// Count one served view on whichever counter its readiness belongs to.
    fn record_view(&mut self, ready: bool) {
        if ready {
            self.stats.ready_views = self.stats.ready_views.saturating_add(1);
        } else {
            self.stats.cold_views = self.stats.cold_views.saturating_add(1);
        }
    }

    /// The wait registration for `anchor`, under the state lock: [`Option::None`]
    /// when no LIVE preparation owns the exact anchor's unsettled generation — it
    /// settled, it is not the driver's current head any more, or nothing has
    /// claimed it — otherwise the live attempt's own completion signal.
    ///
    /// Subscribing is reserved for a genuinely live attempt (the `Preparing`
    /// pose): a delivered head nothing claimed yet, and a head whose claim was
    /// dropped, have no future notification worth waiting for, so the wait
    /// answers terminal instead of parking.
    fn registration(
        &self,
        anchor: ReadAnchor,
        installed: Option<ReadAnchor>,
    ) -> Option<watch::Receiver<Option<HeadCompletion>>> {
        if installed == Some(anchor) {
            return None;
        }
        match self.current.as_ref() {
            Some(CurrentHead::Preparing(head)) if head.delivery.anchor == anchor => {
                Some(head.attempt.subscribe())
            }
            _ => None,
        }
    }

    /// The terminal result the CURRENT attempt on `anchor` published, when the
    /// head the driver holds is that one.
    fn completion(&self, anchor: ReadAnchor) -> Option<HeadCompletion> {
        let current = self.current.as_ref()?;
        if current.anchor() != anchor {
            return None;
        }
        current.attempt().published()
    }

    /// The head the driver holds — settled or still preparing.
    fn delivered(&self) -> Option<ReadAnchor> {
        self.current.as_ref().map(CurrentHead::anchor)
    }

    /// The previous anchor the read window covers, when the installing transition
    /// linked.
    fn previous_anchor(&self) -> Option<ReadAnchor> {
        self.previous.as_ref().map(|previous| previous.anchor)
    }

    /// The report this driver's own apply produced for the head it holds.
    fn last_report(&self) -> Option<Arc<DeltaReport>> {
        self.current.as_ref().and_then(CurrentHead::report)
    }

    /// The lifecycle's cumulative counters.
    fn stats(&self) -> HeadStats {
        self.stats
    }
}

/// The asynchronous canonical-head lifecycle for one [`DeltaSnapshotCache`].
///
/// # Prepare, then apply
///
/// [`Self::deliver`] records a head by its exact anchor and mints the cold
/// generation callers read until the apply lands. [`Self::prepare`] fetches and
/// fully decodes that head's BAL — the stage that owns the RPC, so the caller
/// runs it off its critical path — and [`Self::apply`] moves the snapshot in one
/// synchronous transition: the exact lineage classification against the
/// installed generation, the sealed parent chain linked only when the lineage
/// matched and the list decoded, the final writes applied under the caller's
/// relevance set, and the retention/compaction transition of
/// [`DeltaSnapshotCache::advance_bal`] unchanged. [`Self::advance`] is the two
/// stages together, for a caller that owns the whole background phase.
///
/// # What a caller is guaranteed
///
/// - A view at a delivered head is [`HeadReadiness::Cold`] — serving no parent
///   value — until that exact head's prepared list applied. An unsupported,
///   missing, or malformed list is the cold path with the reason
///   reported, never a stale parent value.
/// - A prepared list applies only to the head it was prepared for: the exact
///   anchor is fenced, and a list whose head a later delivery superseded is
///   discarded, so a late response can never install an abandoned branch.
/// - A settled generation is installed under the same exact-hash lineage rule as
///   the synchronous control, so the only ways a head settles cold are the ones
///   `advance_bal` already has (a lineage refusal, an unreadable list, or both).
/// - Reads are never blocked by a pending preparation: [`Self::snapshot`] is
///   synchronous, and a caller that wants to wait owns the await on the
///   preparation itself.
///
/// # Late settles, single flight, and what the cold generation costs
///
/// A caller that already holds a view keeps reading the generation it was handed
/// when the head moves on: the sealed node is immutable, so a settle that lands
/// after the apply stays in the handle that asked (the delta chain's existing
/// late-settle locality). While a head is unprepared every caller at it shares
/// ONE cold generation, so duplicate reads coalesce and distinct misses share
/// one storage batch exactly as they do at any anchored hub; when the head
/// settles, that provisional generation is dropped and values it settled are
/// not carried into the settled one — a candidate that reads the same identity
/// again pays a second dynamic read, which is the price of the cold path, not a
/// correctness question, and a query for that same head after the next one
/// arrived still answers with the generation that settled it. A view at an
/// anchor no head ever answered (never delivered, or an abandoned head) mints a
/// fresh cold generation per query: hold the earlier view for a stable hub.
pub struct BalHeadDriver {
    provider: AlloyProvider,
    cache: DeltaSnapshotCache,
    retention: Retention,
    state: Mutex<HeadLifecycle>,
}

impl BalHeadDriver {
    /// A driver over a fresh cache at [`DEFAULT_MAX_DEPTH`] and
    /// [`Retention::Unbounded`].
    #[must_use]
    pub fn new(provider: &AlloyProvider) -> Self {
        Self::with_config(provider, DEFAULT_MAX_DEPTH, Retention::Unbounded)
    }

    /// A driver whose cache uses `max_depth` and `retention` for every
    /// generation it installs, and whose provisional generations match.
    #[must_use]
    pub fn with_config(provider: &AlloyProvider, max_depth: u32, retention: Retention) -> Self {
        Self {
            provider: provider.clone(),
            cache: DeltaSnapshotCache::with_max_depth_and_retention(provider, max_depth, retention),
            retention,
            state: Mutex::new(HeadLifecycle::default()),
        }
    }

    /// The retention policy every generation this driver installs obeys.
    ///
    /// A reader surface, not a knob: the policy is chosen once at construction
    /// ([`Self::new`] is the library default, [`Self::with_config`] is a
    /// caller's choice), so the selection a session made is checkable without
    /// reproducing a transition.
    #[must_use]
    pub const fn retention(&self) -> Retention {
        self.retention
    }

    /// The compaction threshold every generation this driver installs obeys
    /// (`DEFAULT_MAX_DEPTH` unless the caller passed another).
    ///
    /// The fold horizon is what bounds expired-but-undropped garbage, so the
    /// threshold is part of the retention answer rather than a separate
    /// tuning.
    #[must_use]
    pub const fn max_depth(&self) -> u32 {
        self.cache.max_depth
    }

    /// Deliver one canonical head: its exact anchor and the parent hash its
    /// lineage is verified against.
    ///
    /// Relevance is NOT a delivery property: a head arrives on the block clock,
    /// which has no candidates and therefore no relevance to hand over, and the
    /// set only needs to exist by the time the head's final writes are applied.
    /// It is an argument of [`Self::apply`] for that reason — the applier is
    /// whoever knows the session's relevant identities.
    ///
    /// A delivery identical to the current head — same anchor and same parent
    /// hash — is idempotent: the same ticket comes back and nothing is minted. A
    /// delivery of a head whose anchor the cache already has installed settles on
    /// the spot without any BAL work (a reorg back to the installed block), and
    /// answers that generation with the readiness its own settle derived, so a
    /// head that settled cold stays cold with its reason instead of being
    /// restated as [`HeadReadiness::Ready`] — a redelivery repeats no transition,
    /// so it cannot restate one. Everything else minted a cold generation that
    /// callers at that head read until [`Self::apply`] lands.
    pub fn deliver(&self, anchor: ReadAnchor, parent_hash: B256) -> HeadTicket {
        let mut state = self.state.lock();
        let installed = self.cache.anchor();
        let seq = state.deliver(anchor, parent_hash, installed, || {
            cold_generation(&self.provider, anchor, self.retention)
        });
        HeadTicket { anchor, seq }
    }

    /// Fetch and fully decode `ticket`'s head BAL — the prepared phase, which
    /// moves no snapshot state. The caller owns the future, so this is what runs
    /// off the critical path; the exact-hash fence is enforced by
    /// [`Self::apply`].
    pub async fn prepare(&self, ticket: HeadTicket) -> PreparedBal {
        PreparedBal::prepare(&self.provider, ticket.anchor).await
    }

    /// Apply `prepared` to `ticket`'s head in one synchronous transition, with
    /// `relevance` naming the identities this head's writes may SEED (a write to
    /// a key the generation already serves always overwrites; an unknown key
    /// seeds only when its address is relevant).
    ///
    /// The list applies only when the ticket names the head the driver still
    /// holds and the list was prepared at that same exact anchor; a superseded
    /// ticket and a foreign list install nothing. A head that is already settled
    /// is idempotent: nothing moves and no write is re-applied.
    ///
    /// A successful apply settles the head's CURRENT claim attempt, which is what
    /// [`Self::completion`] and a new [`Self::wait_for_head`] report: a live
    /// attempt publishes its own settle, while a head whose attempt was dropped
    /// (cancelled) before it settled — no retry holding the head, so an apply can
    /// land on a head whose only claim already ended — publishes the settle on a
    /// FRESH private attempt epoch, leaving that abandonment as its own waiters'
    /// answer. The replacement is not public sequencing: the ticket's `seq` is
    /// unchanged and no caller sees a new generation.
    pub fn apply(
        &self,
        ticket: HeadTicket,
        prepared: &PreparedBal,
        relevance: &[Address],
    ) -> HeadOutcome {
        self.apply_members(ticket, prepared, &RelevanceSet::from_slice(relevance))
    }

    /// The transition with the relevance as a membership set the caller already
    /// holds: the session's apply-time relevance is materialized once per
    /// registry revision, so the head pipeline hands the set over and the apply
    /// pays no conversion. [`Self::apply`] is this method with a caller's slice
    /// converted first.
    ///
    /// Membership is the whole difference between the two, and it moves no
    /// decision (pinned by
    /// `the_set_and_slice_relevance_inputs_produce_an_identical_report`).
    pub fn apply_members(
        &self,
        ticket: HeadTicket,
        prepared: &PreparedBal,
        relevance: &RelevanceSet,
    ) -> HeadOutcome {
        let mut state = self.state.lock();
        let plan = state.plan_apply(
            ticket.anchor,
            ticket.seq,
            prepared.anchor(),
            self.cache.anchor(),
        );
        let (anchor, parent_hash, prior) = match plan {
            ApplyPlan::Refused(outcome) => return outcome,
            ApplyPlan::Admitted {
                anchor,
                parent_hash,
                prior,
            } => (anchor, parent_hash, prior),
        };
        // The generation this transition is about to seal, held before the move:
        // `for_anchor` on the installed anchor hands back the installed handle
        // (and never installs a new one), and after the seal that same handle
        // answers every value it served at its own anchor — the parent chain
        // moved into the sealed node the child now owns.
        let prior = prior.map(|installed| (installed, self.cache.for_anchor(installed)));
        let (_hub, report) = self
            .cache
            .apply_prepared(anchor, parent_hash, relevance, prepared);
        let report = Arc::new(report);
        state.record_settle(anchor, &report, prior);
        state.settle_current(Arc::clone(&report));
        HeadOutcome::Settled(report)
    }

    /// The whole background phase: claim this head's preparation, fetch and
    /// decode the list, and apply it — the shape a caller spawns or joins
    /// against its candidate work.
    ///
    /// Only one caller prepares a given head: a second concurrent advance for the
    /// same ticket returns [`HeadOutcome::InFlight`] instead of duplicating the
    /// fetch, and a dropped (cancelled) advance releases the claim so a later
    /// call prepares the head normally. A settled head repeats no work.
    pub async fn advance(&self, ticket: HeadTicket, relevance: &[Address]) -> HeadOutcome {
        self.advance_with_relevance(ticket, || Arc::new(RelevanceSet::from_slice(relevance)))
            .await
    }

    /// [`Self::advance`] with the head's relevance resolved by `resolve` AFTER
    /// the list is prepared and immediately BEFORE it applies.
    ///
    /// That point is the whole reason this entry exists. Apply-time relevance
    /// is a property of the SESSION, not of the head's producer, and it is read
    /// as late as the transition allows: a path registered while the list was
    /// still in flight is included in this head's seeding, and a path registered
    /// after the apply seeds from the NEXT head (reading its own unknown slots
    /// dynamically until then). `resolve` runs at most once per head, inside the
    /// single-flight claim, so a repeat delivery that loses the claim never
    /// derives anything.
    pub async fn advance_with_relevance<F>(&self, ticket: HeadTicket, resolve: F) -> HeadOutcome
    where
        F: FnOnce() -> Arc<RelevanceSet>,
    {
        let epoch = {
            let mut state = self.state.lock();
            match state.claim(ticket.anchor, ticket.seq, self.cache.anchor()) {
                ClaimStart::Claimed(epoch) => epoch,
                ClaimStart::Refused(outcome) => return outcome,
            }
        };
        let claim = PrepareClaim {
            driver: self,
            seq: ticket.seq,
            epoch,
        };
        let prepared = self.prepare(ticket).await;
        // The relevance is read HERE: after the fetch landed, before the
        // transition. Anything that registered during the fetch window is in
        // the set this head applies with.
        let relevance = resolve();
        // The claim outlives the apply: its drop publishes the abandonment
        // only when the apply settled nothing, so a cancelled preparation and
        // a settled one stay distinguishable to the generation's waiters.
        let outcome = self.apply_members(ticket, &prepared, &relevance);
        drop(claim);
        outcome
    }

    /// The snapshot `anchor` answers with right now, never blocking.
    ///
    /// Four answers, in this order:
    ///
    /// 1. the installed generation whenever that exact anchor settled — which
    ///    keeps answering after a later head was delivered;
    /// 2. the current head's cold generation while its BAL is still preparing;
    /// 3. the PREVIOUS anchor's generation, when the transition that installed
    ///    the current head linked (see [`Self::previous`] — this is the window a
    ///    fan-out whose candidates were solved one block behind the head reads
    ///    through, and it answers with the readiness that generation reported);
    /// 4. a fresh cold generation with the reason (`Superseded` /
    ///    `Undelivered`).
    pub fn snapshot(&self, anchor: ReadAnchor) -> HeadView {
        let mut state = self.state.lock();
        let lookup = state.lookup(anchor, self.cache.anchor());
        let (hub, readiness) = match lookup {
            Lookup::Installed { readiness } | Lookup::Settled { readiness } => {
                (self.cache.for_anchor(anchor), readiness)
            }
            Lookup::Unprepared { hub } => (hub, HeadReadiness::Cold(HeadCold::Unprepared)),
            Lookup::Previous { hub, readiness } => (hub, readiness),
            Lookup::Cold { reason } => (
                cold_generation(&self.provider, anchor, self.retention),
                HeadReadiness::Cold(reason),
            ),
        };
        state.record_view(readiness.is_ready());
        HeadView { hub, readiness }
    }

    /// The head the driver holds — settled or still preparing.
    #[must_use]
    pub fn delivered(&self) -> Option<ReadAnchor> {
        self.state.lock().delivered()
    }

    /// The generation the cache has installed, i.e. the last head that settled.
    #[must_use]
    pub fn installed(&self) -> Option<ReadAnchor> {
        self.cache.anchor()
    }

    /// The previous anchor the read window covers: the generation the installed
    /// head replaced, when the installing transition linked. `None` after a gap,
    /// the first head, or a reorg — the window is two consecutive canonical
    /// anchors or one, never a policy.
    #[must_use]
    pub fn previous(&self) -> Option<ReadAnchor> {
        self.state.lock().previous_anchor()
    }

    /// The report the driver's own apply produced for the head it holds, when it
    /// applied one. A head that settled without this driver's apply (a delivery
    /// of an already-installed anchor) has none.
    #[must_use]
    pub fn last_report(&self) -> Option<Arc<DeltaReport>> {
        self.state.lock().last_report()
    }

    /// The lifecycle's cumulative counters.
    #[must_use]
    pub fn stats(&self) -> HeadStats {
        self.state.lock().stats()
    }

    /// The terminal result the CURRENT claim attempt on `anchor` published, when
    /// the head the driver holds is that one and that attempt has published one
    /// — the completion channel behind [`Self::wait_for_head`], readable without
    /// waiting.
    ///
    /// A head claimed again after an abandonment answers with the new attempt's
    /// result once that attempt ends (`None` while it is undecided), so an
    /// abandonment an earlier attempt published stays that attempt's waiters'
    /// answer without being the head's current one. An out-of-band
    /// [`Self::apply`] that settles the head without any retry claims it answers
    /// `Settled` the same way: the settle mints a fresh private attempt epoch on
    /// the same public ticket, because the head's current claim moves to the
    /// settled truth rather than the abandonment being rewritten.
    #[must_use]
    pub fn completion(&self, anchor: ReadAnchor) -> Option<HeadCompletion> {
        self.state.lock().completion(anchor)
    }

    /// Wait until `anchor` reaches a terminal state, then answer it: the
    /// deterministic wait a same-block caller joins the head's EXISTING
    /// preparation with (see the module docs).
    ///
    /// The wait registers on the exact `(anchor, seq)` generation's CURRENT
    /// claim attempt — only while that attempt's preparation is genuinely live —
    /// and resolves on that attempt's completion notification, then RE-READS the
    /// exact anchor. Completion is attempt-scoped, so a retry that settles the
    /// same anchor never rewrites the abandonment this wait joined: the
    /// abandonment is terminal for the attempt that published it, and the
    /// retry's settle is the next attempt's result. The returned view is never
    /// [`HeadCold::Unprepared`] and never parks without a live owner: a settle
    /// answers the settled generation ([`HeadReadiness::Ready`]) or the
    /// settled-cold reason, a supersession answers [`HeadCold::Superseded`], and
    /// a generation no live preparation owns — dropped mid-flight, or delivered
    /// and never claimed — answers [`HeadCold::Abandoned`] at that exact anchor,
    /// decided once and never retroactively changed by a later retry. It fetches
    /// nothing, claims nothing, applies nothing, and holds no lock across the
    /// wait; many waiters share one attempt's one completion, and dropping one
    /// wait cancels nothing. The wait ends on those real outcomes — never on a
    /// wall clock.
    pub async fn wait_for_head(&self, anchor: ReadAnchor) -> HeadView {
        loop {
            let registered = {
                let state = self.state.lock();
                state.registration(anchor, self.cache.anchor())
            };
            if let Some(mut signal) = registered {
                // The subscription was taken under the same lock hold that
                // read the head's state, so a notification landing between the
                // two is still seen here — there is no lost-wakeup window.
                let _ = signal.changed().await;
                // The wake is this generation's end being published. An
                // abandonment is the wait's own terminal answer: it resolves
                // here rather than registering again onto the published
                // abandonment — a retry that settles later serves NEW
                // callers, never this one retroactively.
                if matches!(*signal.borrow(), Some(HeadCompletion::Abandoned)) {
                    return self.abandoned_view(anchor);
                }
                continue;
            }
            let view = self.snapshot(anchor);
            return match &view.readiness {
                // The exact head is current and unsettled with no live
                // preparation: its terminal answer is the abandoned cold
                // view, never a park.
                HeadReadiness::Cold(HeadCold::Unprepared) => HeadView {
                    hub: view.hub,
                    readiness: HeadReadiness::Cold(HeadCold::Abandoned),
                },
                _ => view,
            };
        }
    }

    /// The wait's terminal answer for a generation no live preparation owns:
    /// the exact anchor's cold generation — carrying nothing, never another
    /// head's values — with the closed `abandoned` reason.
    fn abandoned_view(&self, anchor: ReadAnchor) -> HeadView {
        let view = self.snapshot(anchor);
        let hub = if view.readiness.is_ready() {
            // A retry settled while this wait was waking. The settled
            // generation is the anchor's truth for NEW callers, but this
            // wait's answer is the abandonment it observed, and a cold answer
            // carries nothing — so it gets its own cold generation.
            cold_generation(&self.provider, anchor, self.retention)
        } else {
            view.hub
        };
        HeadView {
            hub,
            readiness: HeadReadiness::Cold(HeadCold::Abandoned),
        }
    }
}

/// The ownership of one head's preparation ATTEMPT: dropping it — a cancelled
/// advance included — releases the claim so a later call prepares that head
/// normally.
struct PrepareClaim<'a> {
    driver: &'a BalHeadDriver,
    seq: u64,
    /// The attempt this claim took. A released attempt publishes only on its own
    /// epoch's channel, so it can never write over the attempt that replaced it.
    epoch: u64,
}

impl Drop for PrepareClaim<'_> {
    fn drop(&mut self) {
        // The attempt's end is terminal for the waiters that joined it: they wake
        // to the abandoned cold answer instead of sleeping through a preparation
        // that is gone. Any terminal this attempt already published is its last
        // word and stays, so a release that no longer owns the head's live
        // attempt leaves the pose — and that attempt's result — alone.
        self.driver.state.lock().release(self.seq, self.epoch);
    }
}

/// A cold generation at `anchor`: pinned at the caller's own anchor, carrying
/// nothing, so no value can come from another head.
fn cold_generation(
    provider: &AlloyProvider,
    anchor: ReadAnchor,
    retention: Retention,
) -> Arc<DeltaHub> {
    Arc::new(DeltaHub::fresh(provider, anchor, retention))
}

/// Whether a settled head's report says its generation carries parent values.
fn settled_readiness(report: Option<&DeltaReport>) -> HeadReadiness {
    match report.map(|report| &report.outcome) {
        Some(CarryOutcome::Cold { lineage, bal }) => HeadReadiness::Cold(HeadCold::Settled {
            lineage: lineage.clone(),
            bal: bal.clone(),
        }),
        Some(CarryOutcome::Carried { .. } | CarryOutcome::AlreadyCurrent) | None => {
            HeadReadiness::Ready
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One storage identity: one word of one account.
    fn slot(tag: u8, index: u64) -> Slot {
        (Address::new([tag; 20]), U256::from(index))
    }

    /// One storage word.
    fn value(word: u64) -> StorageValue {
        U256::from(word)
    }

    /// The identities every pose is probed for: a local value, a shadowed
    /// value, an inherited value, a late settle, and an identity nobody holds.
    fn probes() -> [Slot; 5] {
        [
            slot(0xaa, 1),
            slot(0xaa, 2),
            slot(0xbb, 1),
            slot(0xcc, 1),
            slot(0xdd, 1),
        ]
    }

    /// The node a seal derives from a generation's own maps: the delta with one
    /// recency record per value, and the `new_keys` count of identities the
    /// parent chain does not already hold.
    fn node(
        parent: Option<Arc<DeltaSnapshot>>,
        values: &[(Slot, StorageValue)],
        used: &[(Slot, u64)],
    ) -> Arc<DeltaSnapshot> {
        let delta: HashMap<Slot, StorageValue> = values.iter().copied().collect();
        let new_keys = delta
            .keys()
            .filter(|key| parent.as_ref().is_none_or(|parent| !parent.holds(key)))
            .count();
        Arc::new(DeltaSnapshot::node(
            parent,
            delta,
            used.iter().copied().collect(),
            new_keys,
        ))
    }

    /// A generation's own state over `node`: its values and records on top of
    /// the chain, with `new_keys` counted the way a settle and a seeded write
    /// count it. `sealed` marks the pose a child's link leaves behind, where
    /// `node` already holds `values`.
    fn state(
        node: Arc<DeltaSnapshot>,
        values: &[(Slot, StorageValue)],
        used: &[(Slot, u64)],
        sealed: bool,
    ) -> GenState {
        let own: HashMap<Slot, StorageValue> = values.iter().copied().collect();
        let new_keys = own.keys().filter(|key| !node.holds(key)).count();
        GenState {
            values: own,
            used: used.iter().copied().collect(),
            node,
            sealed,
            new_keys,
        }
    }

    /// One chain pose and what it must answer, in [`probes`] order, plus the
    /// identities it can serve.
    struct Pose {
        label: &'static str,
        generation: GenState,
        answers: [Option<StorageValue>; 5],
        live: usize,
    }

    /// The poses a chain can be in: a first head over the void root, a
    /// generation holding its own value over a two-link chain, a generation
    /// whose whole chain was folded into one root, a sealed generation
    /// answering a late settle, and a generation refreshing an inherited word.
    fn poses() -> [Pose; 5] {
        let [local, shadowed, inherited, late, ..] = probes();

        let root = node(
            None,
            &[(shadowed, value(8)), (inherited, value(7))],
            &[(shadowed, 10), (inherited, 10)],
        );

        // A two-link chain: the tip's own value plus a record for an inherited
        // word it was read at — a refresh that carries no value of its own.
        let tip = node(
            Some(Arc::clone(&root)),
            &[(local, value(5))],
            &[(local, 11), (inherited, 12)],
        );

        // A fold of several generations into one root: no parent, many entries.
        let compacted = node(
            None,
            &[(local, value(5)), (inherited, value(7))],
            &[(local, 11), (inherited, 12)],
        );

        // The pose a child's link leaves: the sealed node holds everything this
        // generation had published, and a settle that landed afterwards stays
        // handle-local.
        let sealed = node(
            Some(Arc::clone(&root)),
            &[(local, value(5)), (shadowed, value(9))],
            &[(local, 11), (shadowed, 13)],
        );

        [
            Pose {
                label: "a first head over the void root",
                generation: state(Arc::new(DeltaSnapshot::root()), &[], &[], false),
                answers: [None; 5],
                live: 0,
            },
            Pose {
                label: "a generation holding its own value over a two-link chain",
                generation: state(
                    Arc::clone(&tip),
                    &[(shadowed, value(9))],
                    &[(shadowed, 13), (local, 13), (inherited, 13)],
                    false,
                ),
                answers: [Some(value(5)), Some(value(9)), Some(value(7)), None, None],
                live: 3,
            },
            Pose {
                label: "a generation whose whole chain was compacted into one root",
                generation: state(
                    Arc::clone(&compacted),
                    &[(shadowed, value(9))],
                    &[(shadowed, 13)],
                    false,
                ),
                answers: [Some(value(5)), Some(value(9)), Some(value(7)), None, None],
                live: 3,
            },
            Pose {
                label: "a sealed generation answering its own late settle",
                generation: state(
                    Arc::clone(&sealed),
                    &[(late, value(4))],
                    &[(late, 20)],
                    true,
                ),
                answers: [
                    Some(value(5)),
                    Some(value(9)),
                    Some(value(7)),
                    Some(value(4)),
                    None,
                ],
                live: 4,
            },
            Pose {
                label: "a generation refreshing an inherited word with no value of its own",
                generation: state(Arc::clone(&root), &[], &[(inherited, 20)], false),
                answers: [None, Some(value(8)), Some(value(7)), None, None],
                live: 2,
            },
        ]
    }

    /// The unbounded read fast path answers exactly what the retention-policy
    /// path answers in every chain pose: a local hit, an ancestor hit, a
    /// shadowed value, a miss, a compacted root, and a sealed generation's late
    /// settle.
    ///
    /// The reading generation is far past every record in these fixtures, so a
    /// path that consulted recency would have something to expire.
    #[test]
    fn the_unbounded_read_answers_exactly_what_the_policy_path_answers() {
        let now = 40;

        for pose in &poses() {
            for (key, expected) in probes().iter().zip(pose.answers) {
                let fast = pose.generation.resolved(key, now, Retention::Unbounded);
                assert_eq!(
                    fast,
                    pose.generation
                        .resolved_through_policy(key, now, Retention::Unbounded),
                    "{}: the fast read disagreed with the policy path for {key:?}",
                    pose.label
                );
                assert_eq!(fast, expected, "{}: {key:?}", pose.label);
            }
        }
    }

    /// The unbounded live count answers exactly what the retention-policy path
    /// counts in every chain pose, and equals the snapshot count: with nothing
    /// expiring, every held identity is a served one.
    #[test]
    fn the_unbounded_live_count_answers_exactly_what_the_policy_path_counts() {
        let now = 40;

        for pose in &poses() {
            let live = pose.generation.live_len(now, Retention::Unbounded);
            assert_eq!(
                live,
                pose.generation
                    .live_len_through_policy(now, Retention::Unbounded),
                "{}: the fast live count disagreed with the policy path",
                pose.label
            );
            assert_eq!(live, pose.live, "{}: the live count", pose.label);
            assert_eq!(
                live,
                pose.generation
                    .node
                    .entries()
                    .saturating_add(pose.generation.new_keys),
                "{}: the fast live count is the snapshot count",
                pose.label
            );
        }
    }

    /// Idle retention still expires what the unbounded path serves: the fast
    /// paths are the unbounded branch only, so an entry past its window stays
    /// unknown and uncounted at the reading generation.
    #[test]
    fn idle_retention_still_expires_what_the_unbounded_read_path_serves() {
        let stale = slot(0xaa, 1);
        let fresh = slot(0xbb, 1);
        let base = node(
            None,
            &[(stale, value(7)), (fresh, value(8))],
            &[(stale, 10), (fresh, 40)],
        );
        let generation = state(base, &[], &[], false);

        // Far past the stale entry's window, one block into the fresh one's.
        let now = 41;

        assert_eq!(
            generation.resolved(&stale, now, Retention::Unbounded),
            Some(value(7)),
            "unbounded retains every settled word"
        );
        assert_eq!(
            generation.resolved(&stale, now, Retention::IdleBlocks(0)),
            None,
            "an entry past its window is unknown again"
        );
        assert_eq!(
            generation.resolved(&fresh, now, Retention::IdleBlocks(10)),
            Some(value(8)),
            "an entry inside its window is still served"
        );
        assert_eq!(generation.live_len(now, Retention::Unbounded), 2);
        assert_eq!(
            generation.live_len(now, Retention::IdleBlocks(10)),
            1,
            "only the entry inside its window is counted"
        );
        assert_eq!(generation.live_len(now, Retention::IdleBlocks(0)), 0);
    }
}
