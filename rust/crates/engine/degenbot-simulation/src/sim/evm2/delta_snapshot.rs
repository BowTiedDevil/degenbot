//! `delta_snapshot` — the parent-linked delta-chain snapshot prototype: one
//! generation's snapshot is its parent's plus a small delta, and a bounded
//! chain of them is periodically compacted.
//!
//! The production carry-forward ([`ReadHubCache::advance_bal`](super::read_hub::ReadHubCache::advance_bal))
//! clones the parent's whole settled-storage map into every child. That is the
//! full-clone CONTROL this prototype is measured against, and it is unchanged:
//! this module is an additive alternative representation consumed only by
//! tests and examples. It answers the same transition questions — what a child
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
//! rule. Only settled storage words cross between generations. Accounts,
//! waiters, pending reads, stats, and EVM overlays keep the existing semantics
//! untouched — this prototype owns no dispatch, no coalescing, no retention
//! policy, and no persistent-map/copy-on-write structure; those are separate
//! workstreams.
//!
//! # Development and migration notes
//!
//! A consumer opting in swaps its snapshot owner for a [`DeltaSnapshotCache`]
//! and its per-anchor read handle for a [`DeltaHub`]: `for_anchor` and
//! `advance_bal` keep the signatures and semantics the hub cache taught it
//! (same [`ReadAnchor`] pinning, same [`CarryOutcome`](super::read_hub::CarryOutcome)
//! / [`CarryRefusal`](super::read_hub::CarryRefusal) reports), and
//! [`DeltaHub::read_storage`] is the dynamic read seam — a chain hit answers
//! on the spot, a miss is the provider's serial slot read at the anchor and
//! settles into the generation's own map. What a migrating consumer must
//! decide for itself: reads at one generation coalesce through the production
//! hub today (single-flight, batched rounds) and this prototype deliberately
//! does not reimplement that; a production integration would route the miss
//! through the hub's round machinery rather than a serial fetch. The
//! representation question — whether the delta chain beats a full clone at
//! realistic working-set sizes — is what
//! `just bal-delta-snapshot-probe` measures (knobs: `BAL_DELTA_SIZES`,
//! `BAL_DELTA_REPS`, `BAL_DELTA_LOOKUPS`, `BAL_DELTA_PLANT_DIVERGENCE`), with
//! the absolute measurements and the go/no-go for a copy-on-write workstream
//! recorded in `logs/bal_delta_snapshot_result.md`.

use std::collections::HashMap;

use std::sync::Arc;

use std::time::{Duration, Instant};

use alloy::primitives::{Address, StorageValue, B256, U256};

use parking_lot::Mutex;

use degenbot_core::errors::ProviderResult;

use degenbot_rpc::provider::AlloyProvider;

use super::bal_prefetch::{decode_bal_seed, BalPlan, BalSkip};

use super::read_hub::{classify_link, CarryOutcome};

use crate::sim::storage_memo::ReadAnchor;

/// One storage read identity: one word of one account at a generation's
/// anchor.
pub type Slot = (Address, U256);

/// The default compaction threshold: the ancestor links any one node may
/// carry before its next child compacts them into one root.
pub const DEFAULT_MAX_DEPTH: u32 = 8;

/// One immutable generation node: this generation's delta over its parent
/// chain.
///
/// A node's [`delta`](Self::delta_len) holds exactly what its generation
/// contributed — final writes applied at the transition and values its dynamic
/// reads settled — never a copy of an ancestor's map. [`Self::get`] resolves
/// the delta before the ancestors, which is the final-write precedence the
/// full-clone control gets from one flat map.
pub struct DeltaSnapshot {
    parent: Option<Arc<DeltaSnapshot>>,

    delta: HashMap<Slot, StorageValue>,

    depth: u32,

    entries: usize,
}

impl DeltaSnapshot {
    /// The void root a fresh generation links against: no values, no ancestors.
    fn root() -> Self {
        Self {
            parent: None,

            delta: HashMap::new(),

            depth: 0,

            entries: 0,
        }
    }

    /// A node over `parent` carrying `delta`, of which `new_keys` identities
    /// the parent chain does not already hold.
    fn node(
        parent: Option<Arc<DeltaSnapshot>>,

        delta: HashMap<Slot, StorageValue>,

        new_keys: usize,
    ) -> Self {
        let (depth, base_entries) = parent
            .as_ref()
            .map_or((0, 0), |node| (node.depth.saturating_add(1), node.entries));

        Self {
            parent,

            delta,

            depth,

            entries: base_entries.saturating_add(new_keys),
        }
    }

    /// Resolve one identity through this node and its ancestors — the child's
    /// own writes and settles before any ancestor value.
    #[must_use]
    pub fn get(&self, key: &Slot) -> Option<StorageValue> {
        if let Some(value) = self.delta.get(key) {
            return Some(*value);
        }

        self.parent.as_ref().and_then(|parent| parent.get(key))
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

    /// Distinct identities visible from this node: its own delta plus every
    /// ancestor's.
    #[must_use]
    pub const fn entries(&self) -> usize {
        self.entries
    }

    /// Whether this is the void root: nothing to compact, nothing to link.
    fn is_void(&self) -> bool {
        self.parent.is_none() && self.delta.is_empty()
    }

    /// Fold this node's whole ancestor chain into one compacted root: a fresh
    /// map with [`Option::None`] parent, built oldest-generation-first so a
    /// later generation's value wins. Every ancestor keeps its own maps — an
    /// older reader holding any of them is untouched.
    fn compact(&self) -> (Arc<DeltaSnapshot>, CompactionReport) {
        let started = Instant::now();

        let mut chain = Vec::new();

        let mut walk = Some(self);

        while let Some(node) = walk {
            chain.push(node);

            walk = node.parent.as_deref();
        }

        let mut merged = HashMap::with_capacity(self.entries);

        for node in chain.iter().rev() {
            merged.extend(node.delta.iter().map(|(key, value)| (*key, *value)));
        }

        let entries = merged.len();

        let nodes_merged = chain.len();

        let root = Arc::new(DeltaSnapshot::node(None, merged, entries));

        (
            root,
            CompactionReport {
                nodes_merged,

                entries,

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

    /// Wall time of the fold.
    pub elapsed: Duration,
}

/// What one application of a child's final writes did.
struct AppliedWrites {
    /// The identities applied, in BAL order.
    applied: Vec<(Address, U256)>,

    /// How many applied writes replaced a value the snapshot already held.
    overwritten: usize,

    /// Unknown-slot writes left unknown: address outside the relevance set.
    skipped: usize,

    /// Applied writes to identities no ancestor chain holds.
    new_keys: usize,
}

/// Apply the child BAL's final writes to a fresh delta over `base` under the
/// carry's write policy: a write to a slot the snapshot already holds
/// OVERWRITES it whatever `relevance` says (a stale carried value outliving
/// the block that changed it is the one outcome the carry must never produce,
/// a zero or deletion final included), and a write to an UNKNOWN slot seeds
/// only when its address is in `relevance` — an irrelevant unknown write stays
/// unknown (counted, never hidden) so a later read takes the dynamic path.
fn seed_final_writes(
    writes: &[(Address, U256, U256)],

    relevance: &[Address],

    base: &DeltaSnapshot,
) -> (HashMap<Slot, StorageValue>, AppliedWrites) {
    let mut values = HashMap::new();

    let mut applied_writes = AppliedWrites {
        applied: Vec::new(),

        overwritten: 0,

        skipped: 0,

        new_keys: 0,
    };

    for &(address, slot, value) in writes {
        let key = (address, slot);

        let known = values.contains_key(&key) || base.get(&key).is_some();

        if !known && !relevance.contains(&address) {
            applied_writes.skipped = applied_writes.skipped.saturating_add(1);

            continue;
        }

        values.insert(key, value);

        if known {
            applied_writes.overwritten = applied_writes.overwritten.saturating_add(1);
        } else {
            applied_writes.new_keys = applied_writes.new_keys.saturating_add(1);
        }

        applied_writes.applied.push(key);
    }

    (values, applied_writes)
}

/// One generation's state: its own values over the chain below them.
struct GenState {
    /// This generation's own values — its delta while it is the chain head,
    /// its handle-local late settles once a child has sealed it.
    values: HashMap<Slot, StorageValue>,

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
    /// Resolve one identity: this generation's own map before the chain.
    fn resolved(&self, key: &Slot) -> Option<StorageValue> {
        self.values.get(key).copied().or_else(|| self.node.get(key))
    }

    /// Record one settled value, counting it only when neither this
    /// generation's map nor the chain below already held the identity.
    fn settle(&mut self, key: Slot, value: StorageValue) {
        let fresh = !self.values.contains_key(&key) && self.node.get(&key).is_none();

        self.values.insert(key, value);

        if fresh {
            self.new_keys = self.new_keys.saturating_add(1);
        }
    }
}

/// One anchor generation's snapshot handle (see the module docs).
///
/// Reads resolve the generation's own map before its ancestors and fall back
/// to the provider for anything the chain does not hold; a settled fallback
/// joins the generation's own map, so it shadows ancestors and carries to the
/// next generation exactly like the control's memo.
pub struct DeltaHub<'p> {
    provider: &'p AlloyProvider,

    anchor: ReadAnchor,

    state: Mutex<GenState>,
}

impl<'p> DeltaHub<'p> {
    /// A generation over `base` whose own map starts at `values`.
    fn generation(
        provider: &'p AlloyProvider,

        anchor: ReadAnchor,

        base: Arc<DeltaSnapshot>,

        values: HashMap<Slot, StorageValue>,

        new_keys: usize,
    ) -> Self {
        Self {
            provider,

            anchor,

            state: Mutex::new(GenState {
                values,

                node: base,

                sealed: false,

                new_keys,
            }),
        }
    }

    /// A fresh cold generation: nothing carried, nothing written.
    fn fresh(provider: &'p AlloyProvider, anchor: ReadAnchor) -> Self {
        Self::generation(
            provider,
            anchor,
            Arc::new(DeltaSnapshot::root()),
            HashMap::new(),
            0,
        )
    }

    /// The anchor every value this handle serves is pinned to.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The snapshot's own answer for one identity — [`Option::None`] while the
    /// identity is still unknown to this generation and every ancestor.
    #[must_use]
    pub fn snapshot_value(&self, address: Address, index: U256) -> Option<StorageValue> {
        let state = self.state.lock();

        state.resolved(&(address, index))
    }

    /// Distinct identities the snapshot holds: this generation's own map plus
    /// every ancestor's, the control's `memoized_slots` counterpart.
    #[must_use]
    pub fn snapshot_len(&self) -> usize {
        let state = self.state.lock();

        state.node.entries().saturating_add(state.new_keys)
    }

    /// How many identities this generation itself contributed — its own delta
    /// while it is the head, its sealed delta plus its late settles after a
    /// child sealed it. A delta child near this number and a `snapshot_len`
    /// near its parent's is the no-full-copy shape.
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

    /// Read one storage word at this generation's anchor.
    ///
    /// A chain hit answers on the spot; a miss is the provider's serial slot
    /// read at the anchor, and its answer settles into the generation's own
    /// map so the next read — here or at any child — is a hit. The coalescing,
    /// waiter, and stats semantics stay the production hub's (module docs).
    ///
    /// # Errors
    ///
    /// The dynamic fallback's serial read failed — a refused slot or a
    /// transport failure — and nothing was settled; a later read retries.
    pub async fn read_storage(
        &self,
        address: Address,
        index: U256,
    ) -> ProviderResult<StorageValue> {
        let key = (address, index);

        if let Some(value) = self.snapshot_value(address, index) {
            return Ok(value);
        }

        let fetched = self.fetch_one(address, index).await?;

        let mut state = self.state.lock();

        state.settle(key, fetched);

        Ok(fetched)
    }

    /// The dynamic fallback's one serial read, pinned to this anchor.
    async fn fetch_one(&self, address: Address, index: U256) -> ProviderResult<StorageValue> {
        let word = self
            .provider
            .get_storage_at(&address, index, Some(self.anchor.number))
            .await?;

        Ok(U256::from_be_bytes(word.0))
    }

    /// Seal this generation's own map into an immutable node over a base
    /// compacted to the threshold, and return it — the link a child walks.
    ///
    /// The move is a take, never a copy: whatever this generation settles
    /// afterwards lands in its late map and stays invisible to every child,
    /// the exact visibility boundary of the control's clone-at-transition.
    fn seal(&self, max_depth: u32) -> (Arc<DeltaSnapshot>, Option<CompactionReport>) {
        let mut state = self.state.lock();

        if state.sealed {
            return (Arc::clone(&state.node), None);
        }

        let (base, compaction) = compact_base(Arc::clone(&state.node), max_depth);

        let delta = std::mem::take(&mut state.values);

        let new_keys = std::mem::take(&mut state.new_keys);

        let parent = if base.is_void() { None } else { Some(base) };

        let sealed = Arc::new(DeltaSnapshot::node(parent, delta, new_keys));

        state.node = Arc::clone(&sealed);

        state.sealed = true;

        (sealed, compaction)
    }
}

/// Compact `base` when linking one more generation would push past
/// `max_depth`, so every node stays within the documented walk bound.
fn compact_base(
    base: Arc<DeltaSnapshot>,

    max_depth: u32,
) -> (Arc<DeltaSnapshot>, Option<CompactionReport>) {
    if base.depth().saturating_add(1) > max_depth {
        let (root, report) = base.compact();

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

    /// How many of `writes` replaced a value the snapshot already held.
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
}

impl DeltaReport {
    /// The no-op report of a same-anchor repeat: no BAL work, no link.
    fn already_current(bal_latency: Duration) -> Self {
        Self {
            outcome: CarryOutcome::AlreadyCurrent,

            writes: Vec::new(),

            writes_overwritten: 0,

            writes_skipped: 0,

            bal_latency,

            link_time: Duration::ZERO,

            compaction: None,
        }
    }
}

/// The session-scoped owner of the per-generation [`DeltaHub`] — the
/// delta-chain counterpart of the control's `ReadHubCache`.
pub struct DeltaSnapshotCache<'p> {
    provider: &'p AlloyProvider,

    max_depth: u32,

    current: Mutex<Option<(ReadAnchor, Arc<DeltaHub<'p>>)>>,
}

impl<'p> DeltaSnapshotCache<'p> {
    /// An empty owner at [`DEFAULT_MAX_DEPTH`].
    #[must_use]
    pub fn new(provider: &'p AlloyProvider) -> Self {
        Self::with_max_depth(provider, DEFAULT_MAX_DEPTH)
    }

    /// An empty owner whose compaction threshold bounds every node's
    /// [`depth`](DeltaSnapshot::depth). A threshold below 1 is treated as 1:
    /// every generation then keeps at most its own delta over one compacted
    /// root.
    #[must_use]
    pub fn with_max_depth(provider: &'p AlloyProvider, max_depth: u32) -> Self {
        Self {
            provider,

            max_depth: max_depth.max(1),

            current: Mutex::new(None),
        }
    }

    /// The generation for `anchor`: the installed one when the whole anchor
    /// (number AND hash) is unchanged, otherwise a fresh cold generation that
    /// becomes the installed one — the control's `for_anchor` contract, where
    /// a replacement drops the retired generation's values with it.
    pub fn for_anchor(&self, anchor: ReadAnchor) -> Arc<DeltaHub<'p>> {
        let mut current = self.current.lock();

        if let Some((installed, hub)) = current.as_ref() {
            if *installed == anchor {
                return Arc::clone(hub);
            }
        }

        let hub = Arc::new(DeltaHub::fresh(self.provider, anchor));

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
    /// no parent values), the write policy is the control's (update known
    /// keys, seed only relevant new ones), and a same-anchor call is
    /// idempotent and repeats no BAL work.
    pub async fn advance_bal(
        &self,

        child: ReadAnchor,

        child_parent_hash: B256,

        relevance: &[Address],
    ) -> (Arc<DeltaHub<'p>>, DeltaReport) {
        // Idempotence precedes any RPC: the same child anchor repeats no BAL
        // work, and no lock guard crosses the fetch.
        {
            let current = self.current.lock();

            if let Some((installed, hub)) = current.as_ref() {
                if *installed == child {
                    return (
                        Arc::clone(hub),
                        DeltaReport::already_current(Duration::ZERO),
                    );
                }
            }
        }

        // The child BAL is fetched and FULLY decoded before anything links: a
        // skip here can never return a child serving parent values.
        let bal_started = Instant::now();

        let (final_writes, bal) = self.fetch_child_writes(child).await;

        let bal_latency = bal_started.elapsed();

        // Link, seal, and install under one lock hold, so the lineage check
        // and the anchor move are one transition — and the link seals the same
        // installed parent the check validated.
        let mut current = self.current.lock();

        if let Some((installed, hub)) = current.as_ref() {
            if *installed == child {
                return (Arc::clone(hub), DeltaReport::already_current(bal_latency));
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

                let (sealed, folded) = parent_hub.seal(self.max_depth);

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

        let (values, applied) = seed_final_writes(&final_writes, relevance, &base);

        let hub = Arc::new(DeltaHub::generation(
            self.provider,
            child,
            base,
            values,
            applied.new_keys,
        ));

        let outcome = if linked {
            CarryOutcome::Carried { carried_values }
        } else {
            CarryOutcome::Cold { lineage, bal }
        };

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
            },
        )
    }

    /// The installed anchor (`Option::None` before the first `for_anchor`).
    #[must_use]
    pub fn anchor(&self) -> Option<ReadAnchor> {
        self.current.lock().as_ref().map(|(anchor, _)| *anchor)
    }

    /// The child list's final writes, or the skip that makes the child cold —
    /// the control's fetch/decode stage, wholesale before anything links.
    async fn fetch_child_writes(
        &self,

        child: ReadAnchor,
    ) -> (Vec<(Address, U256, U256)>, Option<BalSkip>) {
        let fetched = match child.hash {
            None => Err(BalSkip::NoAnchorHash),

            Some(hash) => self
                .provider
                .get_raw_block_access_list(hash)
                .await
                .map_err(|error| BalSkip::Rpc(error.to_string())),
        };

        match fetched {
            Err(skip) => (Vec::new(), Some(skip)),

            // Carry-forward is write-only: read-only identities are not
            // prefetched here; unknown reads take the dynamic path.
            Ok(raw) => match decode_bal_seed(&raw, &BalPlan::default()) {
                Err(error) => (Vec::new(), Some(BalSkip::Decode(error.to_string()))),

                Ok(seed) => (seed.writes, None),
            },
        }
    }
}
