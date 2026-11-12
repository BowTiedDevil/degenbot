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
//! The canonical-head lifecycle built over this cache — delivery, the separated
//! prepare phase, the synchronous apply, the wait, and the readiness a settled
//! head reports — lives in [`head_lifecycle`](super::head_lifecycle); this module
//! owns the snapshot representation, the cache, and the synchronous control that
//! lifecycle drives.
//!
//! # Development and migration notes
//!
//! Reading an installed generation and installing one are two calls, because
//! they are two decisions: [`DeltaSnapshotCache::installed_hub`] is the READ
//! (exact anchor, the cache's own handle cloned, [`Option::None`] when no
//! generation is installed there), while `for_anchor`, `advance_bal`, and
//! [`BalHeadDriver::apply`] are the INSTALLS. A consumer that reached for
//! `for_anchor` only to read the current generation switches to `installed_hub`
//! and changes nothing else; a lifecycle/cache pair that disagrees then answers
//! cold instead of quietly replacing the generation that was asked about.
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

use degenbot_rpc::provider::{AccountRead, AlloyProvider};

use super::read_hub::{
    classify_link, CarryOutcome, PreparedBal, ReadFuture, ReadHub, ReadHubError, ReadHubStats,
    RoundReport,
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
    pub(super) fn fresh(
        provider: &AlloyProvider,
        anchor: ReadAnchor,
        retention: Retention,
    ) -> Self {
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

    /// The compaction threshold every installed generation obeys. Read by the
    /// head lifecycle's driver, which reports and reuses it.
    pub(super) max_depth: u32,

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
    ///
    /// This is the EXPLICIT installing control: a mismatched anchor REPLACES the
    /// installed generation and retires the old one's values. A caller that only
    /// wants the generation the cache already has takes [`Self::installed_hub`]
    /// instead, which clones that handle or answers [`Option::None`] and never
    /// installs, replaces, or evicts anything.
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
        if let Some(hub) = self.installed_hub(child) {
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

    /// The generation the cache has installed at exactly `anchor`, when there is
    /// one — the read-only counterpart of [`Self::for_anchor`].
    ///
    /// Clones the existing handle and touches nothing else: an anchor the cache
    /// does not hold installed answers [`Option::None`] instead of minting a
    /// generation, so this lookup can never install, replace, or evict one. A
    /// caller that needs a generation to exist asks for one explicitly
    /// ([`Self::for_anchor`] for a cold one, `apply_prepared` for a linked one);
    /// a reader never does.
    #[must_use]
    pub fn installed_hub(&self, anchor: ReadAnchor) -> Option<Arc<DeltaHub>> {
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
    pub(super) fn apply_prepared(
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

/// The lifecycle's public surface, owned by [`head_lifecycle`](super::head_lifecycle)
/// and reexported here so every existing driver and type path is unchanged.
pub use super::head_lifecycle::{
    BalHeadDriver, HeadCold, HeadCompletion, HeadOutcome, HeadReadiness, HeadStats, HeadTicket,
    HeadView,
};

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
