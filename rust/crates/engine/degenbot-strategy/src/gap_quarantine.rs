//! Frame-liveness FSM: wire frames whose claimed nonce sits ahead
//! of the parent state's sender nonce are TRACKED until the chain proves the
//! frame's opportunity is over. The surviving decisions are rescue (replay
//! predecessors first), wait, and nonce-consumed (classify the consumption,
//! then wait for finality). The FSM is I/O-free: the caller (the sidecar
//! bin's frame loop) fetches the head nonce, the receipt, and the block
//! evidence and performs the actual replay/hydration.
//!
//! # Liveness is finality, not time
//!
//! A tracked frame is never evicted by a clock or by pool absence. The feed
//! makes no timing guarantee, so a late-arriving pending tx is perfectly
//! valid; senders submit to `MEVBlocker` AND the public mempool independently,
//! builders may include ANY of several same-nonce candidates, and a dormant
//! tx can come alive years later. The ONLY proof a frame's opportunity is
//! over is a nonce consumption (`MinedAt` or `SlotTakenAt`) carried in a
//! FINALIZED block -- a reorg can unwind the consumption and revive the
//! frame at any earlier depth.
//!
//! # States and transitions
//!
//! - `Tracked`: the parked/live/dormant mass. No time limit, no cap, no
//!   pool-absence death.
//! - `Tentative(NonceConsumed)`: the frame's hash mined, or its nonce slot
//!   was consumed by another tx, in a NON-finalized block. The carrying
//!   block + hash are retained for the reorg check.
//! - Finalized-dead: a tombstone written only when a tentative frame's
//!   block is at or below the node's `finalized` tag. The frame leaves the
//!   FSM at that point.
//!
//! Transitions (all event-driven, zero clocks):
//!
//! 1. head advance -> `poll` reports `NonceConsumed` for every tracked frame
//!    whose sender nonce reached the claim; the caller classifies via ONE
//!    receipt probe and calls [`Quarantine::enter_tentative`].
//! 2. reorg check -> [`Quarantine::check_reorg`] revives every tentative
//!    frame whose recorded block hash no longer matches the canonical chain.
//! 3. finality -> [`Quarantine::check_finalized`] tombstones every tentative
//!    frame at or below the node's finalized tag.
//! 4. rescue -> `poll` reports the predecessor set the caller's pool view can
//!    produce; the frame leaves the FSM into the funnel. At the frontier
//!    (head == claim) the set is empty: the whole gap is mined, so replaying
//!    against the current head needs no prefix.
//! 5. still-waiting -> the gap remains invisible; the frame stays tracked
//!    with no expiry.

use std::collections::BTreeMap;

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use serde::{Deserialize, Serialize};

/// The identity every parked frame carries, wire or not: the hash the chain
/// knows it by, the sender whose nonce gap parked it, and the gap's two edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameIdentity {
    pub hash: B256,
    pub from: Address,
    /// The nonce the wire claims for this frame (the gap's upper bound).
    pub claimed_nonce: u64,
    /// The head-state account nonce at capture (`expected`); the predecessor
    /// set is `[expected, claimed)`.
    pub expected_at_capture: u64,
}

impl FrameIdentity {
    /// Gap nonces that must land before the frame validates.
    #[must_use]
    pub fn gap_range(&self) -> Vec<u64> {
        (self.expected_at_capture..self.claimed_nonce).collect()
    }
}

/// Why a park carries no wire. Only the journal's migration fold produces one:
/// a legacy line whose wire fields the current schema could not rebuild keeps
/// its identity and this reason, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingWire {
    /// A legacy park line (pre-versioning, foreign version, or schema-rejected)
    /// whose wire the fold could not rebuild.
    LegacyIncomplete,
}

impl MissingWire {
    /// The stable label the journal rows and trace lines carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyIncomplete => "legacy_incomplete",
        }
    }
}

/// The complete wire a flip-side replay needs: every feed field that is not
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameWire {
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// Chain the frame was observed on (funnel re-entry wire field).
    pub chain_id: u64,
    /// EIP-2718 transaction type (funnel re-entry wire field).
    pub tx_type: u8,
    /// Access list, verbatim (funnel re-entry wire field).
    pub access_list: serde_json::Value,
    /// Feed receive time; forensics only, no liveness clock rides the frame.
    pub received_unix_ms: u64,
    /// The signed wire bytes when the source revealed them; a fq park keeps
    /// them so a rescue can still bundle the target verbatim.
    pub raw_signed_tx: Option<Bytes>,
}

/// A park whose whole wire survived: the only shape a rescue can replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
    /// The identity this wire belongs to.
    pub identity: FrameIdentity,
    /// Every field a replay needs beyond the identity.
    pub wire: FrameWire,
}

impl CompleteFrame {
    /// Rebuild the feed event a funnel re-entry needs. Infallible: a complete
    /// frame carries every wire field by construction, so no sentinel or
    /// fallback value can stand in for one.
    #[must_use]
    pub fn to_event(&self) -> BackrunFeedEvent {
        BackrunFeedEvent {
            chain_id: self.wire.chain_id,
            from: self.identity.from,
            to: self.wire.to,
            value: self.wire.value,
            data: self.wire.data.clone(),
            gas: self.wire.gas,
            max_fee_per_gas: self.wire.max_fee_per_gas,
            max_priority_fee_per_gas: self.wire.max_priority_fee_per_gas,
            nonce: self.identity.claimed_nonce,
            hash: self.identity.hash,
            access_list: self.wire.access_list.clone(),
            tx_type: self.wire.tx_type,
            received_unix_ms: self.wire.received_unix_ms,
            raw_signed_tx: self.wire.raw_signed_tx.clone(),
        }
    }
}

/// Identity only: no wire survived the migration fold, so no event can be
/// rebuilt and this frame can never be rescued into the funnel. It stays
/// tracked (one quiet hold per head advance) until the chain proves its nonce
/// consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegradedFrame {
    /// The identity the fold retained.
    pub identity: FrameIdentity,
    /// Why the wire is gone.
    pub reason: MissingWire,
}

/// A frame parked at its gap boundary: identity plus either the wire a rescue
/// replays or the reason that wire is gone. The variants are the states, so a
/// zero-valued wire field can never be read as degradation and a degraded park
/// cannot fabricate an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkedFrame {
    /// The whole wire survived: rescuable.
    Complete(Box<CompleteFrame>),
    /// Identity only: never rescuable.
    Degraded(DegradedFrame),
}

impl ParkedFrame {
    /// Park a feed event as a complete frame: every wire field came from the
    /// feed, so none is a fabricated default.
    #[must_use]
    pub fn from_event(event: &BackrunFeedEvent, expected_at_capture: u64) -> Self {
        Self::Complete(Box::new(CompleteFrame {
            identity: FrameIdentity {
                hash: event.hash,
                from: event.from,
                claimed_nonce: event.nonce,
                expected_at_capture,
            },
            wire: FrameWire {
                to: event.to,
                value: event.value,
                data: event.data.clone(),
                gas: event.gas,
                max_fee_per_gas: event.max_fee_per_gas,
                max_priority_fee_per_gas: event.max_priority_fee_per_gas,
                chain_id: event.chain_id,
                tx_type: event.tx_type,
                access_list: event.access_list.clone(),
                received_unix_ms: event.received_unix_ms,
                raw_signed_tx: event.raw_signed_tx.clone(),
            },
        }))
    }

    /// A migrated park whose wire did not survive: identity and the reason.
    #[must_use]
    pub const fn degraded(identity: FrameIdentity, reason: MissingWire) -> Self {
        Self::Degraded(DegradedFrame { identity, reason })
    }

    /// The identity every variant carries.
    #[must_use]
    pub const fn identity(&self) -> FrameIdentity {
        match self {
            Self::Complete(frame) => frame.identity,
            Self::Degraded(frame) => frame.identity,
        }
    }

    /// The frame's wire hash.
    #[must_use]
    pub const fn hash(&self) -> B256 {
        self.identity().hash
    }

    /// The sender whose nonce gap parked the frame.
    #[must_use]
    pub const fn from(&self) -> Address {
        self.identity().from
    }

    /// The nonce the wire claims (the gap's upper bound).
    #[must_use]
    pub const fn claimed_nonce(&self) -> u64 {
        self.identity().claimed_nonce
    }

    /// The head-state account nonce at capture (the gap's lower bound).
    #[must_use]
    pub const fn expected_at_capture(&self) -> u64 {
        self.identity().expected_at_capture
    }

    /// The same park retagged with a different wire hash.
    #[must_use]
    pub fn with_hash(mut self, hash: B256) -> Self {
        match &mut self {
            Self::Complete(frame) => frame.identity.hash = hash,
            Self::Degraded(frame) => frame.identity.hash = hash,
        }
        self
    }

    /// Gap nonces that must land before the frame validates.
    #[must_use]
    pub fn gap_range(&self) -> Vec<u64> {
        self.identity().gap_range()
    }

    /// The complete frame, when this park still carries its wire. A rescue can
    /// only be built from this arm; the degraded arm has no event at all.
    #[must_use]
    pub fn complete(&self) -> Option<&CompleteFrame> {
        match self {
            Self::Complete(frame) => Some(frame),
            Self::Degraded(_) => None,
        }
    }

    /// Why the wire is gone, when it is.
    #[must_use]
    pub const fn degraded_reason(&self) -> Option<MissingWire> {
        match self {
            Self::Complete(_) => None,
            Self::Degraded(frame) => Some(frame.reason),
        }
    }

    /// Whether this park carries no wire.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.degraded_reason().is_some()
    }
}

/// The nonce-consumption evidence: WHO consumed the frame's nonce and in
/// which block. Recorded when the head passed the claimed nonce and the
/// caller classified the frame (one receipt probe on the frame's own hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceConsumed {
    /// The frame's own tx mined in `block`.
    MinedAt {
        /// The block carrying the tx.
        block: u64,
        /// That block's hash, for the reorg check.
        block_hash: B256,
    },
    /// A different tx took the nonce slot in `block` (`by` = its hash when
    /// the caller could see it).
    SlotTakenAt {
        /// The block carrying the slot-stealing tx.
        block: u64,
        /// That block's hash, for the reorg check.
        block_hash: B256,
        /// The slot-stealing tx hash, when the caller could resolve it.
        by: Option<B256>,
    },
}

impl NonceConsumed {
    /// The block carrying the consumption.
    #[must_use]
    pub const fn block(self) -> u64 {
        match self {
            Self::MinedAt { block, .. } | Self::SlotTakenAt { block, .. } => block,
        }
    }

    /// The carrying block's hash (the reorg check's recorded value).
    #[must_use]
    pub const fn block_hash(self) -> B256 {
        match self {
            Self::MinedAt { block_hash, .. } | Self::SlotTakenAt { block_hash, .. } => block_hash,
        }
    }

    /// Whether the frame's own tx mined (vs a same-nonce replacement).
    #[must_use]
    pub const fn mined(self) -> bool {
        matches!(self, Self::MinedAt { .. })
    }
}

/// Whether a guard's recorded content identity still matches the current
/// pool-known predecessors. Matching requires content on BOTH sides: an empty
/// stored or incoming content vec cannot prove a change, and a false
/// suppression is a lost rescue (a replayed retry only costs one replay), so
/// either empty side re-arms. Guards are in-memory-only — never reloaded from
/// disk — so no legacy empty-content guard exists to preserve.
fn content_matches(
    guarded: &[B256],
    predecessors: &[u64],
    current: Option<&[(u64, B256)]>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    if guarded.is_empty() || current.is_empty() {
        return false;
    }
    predecessors.iter().zip(guarded).all(|(nonce, hash)| {
        current
            .iter()
            .find(|(n, _)| n == nonce)
            .is_some_and(|(_, current_hash)| current_hash == hash)
    })
}

/// One tracked frame's state. `Tracked` is the default; `Tentative` carries
/// the consumption evidence until finality or revival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    /// Parked, live, or dormant -- no clock touches this state.
    Tracked,
    /// The nonce was consumed in a non-finalized block.
    Tentative(NonceConsumed),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    frame: ParkedFrame,
    state: FrameState,
    /// Last head nonce a degraded hold surfaced for, so the quiet hold is
    /// reported once per head advance instead of once per poll.
    last_hold_head: Option<u64>,
}

/// What one poll-cycle says about one tracked frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineDecision {
    /// Every gap nonce is fetchable from the caller's pool view: hydrate the
    /// predecessors (in the returned ascending order) into the scratch EVM,
    /// then the frame. The frame leaves the FSM.
    Rescue {
        /// Predecessor nonces to hydrate first, ascending.
        predecessors: Vec<u64>,
    },
    /// The head state passed the frame's claimed nonce: the caller must
    /// classify the consumption (one receipt probe) and call
    /// [`Quarantine::enter_tentative`]. The frame stays tracked meanwhile.
    NonceConsumed,
    /// Nothing changed since capture: keep the frame parked (forever, if the
    /// predecessors never surface).
    StillWaiting {
        /// Gap nonces whose tx the pool view still cannot produce.
        unknown: Vec<u64>,
    },
    /// A degraded frame surfaced its quiet hold: identity only, no wire to
    /// replay, so no rescue exists for it. Reported once per head advance;
    /// the frame stays tracked until the chain proves the nonce consumed.
    WireLost {
        /// Why the wire is gone.
        reason: MissingWire,
    },
}

/// Why a pool-pred rescue is known-futile. Recorded only when the caller's
/// outcome proved it: a `GapPending` re-park (the replay could not consume the
/// pool-known predecessors) or a structural predecessor-decode failure. The
/// empty mined-frontier rescue is a different evidence class and is never
/// covered by an `Unhydratable` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RescueGuard {
    /// A `GapPending` re-park: suppress a repeat only while the exact
    /// predecessor list, content identity, AND captured boundary are all
    /// unchanged.
    Reparked {
        predecessors: Vec<u64>,
        /// The hydrated predecessors' tx hashes, in nonce order. A content
        /// replacement at the same nonces re-arms the rescue; an empty vec
        /// (or an empty incoming vec) can never suppress.
        content: Vec<B256>,
        boundary: u64,
    },
    /// A predecessor shape that cannot decode (or a predecessor that cannot
    /// run as fetched): suppress the pool-pred list until the predecessor
    /// content changes.
    Unhydratable {
        predecessors: Vec<u64>,
        content: Vec<B256>,
    },
}

/// The pure FSM core: per-sender FIFOs of tracked and tentative frames.
/// `poll` returns one decision per tracked frame of that sender; non-rescued
/// frames stay parked.
#[derive(Debug, Default)]
pub struct Quarantine {
    pending: BTreeMap<Address, Vec<Entry>>,
    /// Why a rescue for a frame hash is known-futile, recorded by the caller
    /// AFTER the outcome proved it -- never from the rescue decision itself.
    /// The key is the exact predecessor list a rescue would hydrate, so an
    /// unrelated pool transaction that only moves the count lane cannot
    /// re-arm a futile rescue. A transient hydration failure records nothing
    /// (an RPC error is retried on the next tick); a fire clears its own
    /// entry; and every terminal exit drops it.
    repark_guard: BTreeMap<B256, RescueGuard>,
}

impl Quarantine {
    /// Fresh FSM.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a frame as [`FrameState::Tracked`]. Returns the total frame count
    /// (tracked + tentative) after insert.
    pub fn push(&mut self, frame: ParkedFrame) -> usize {
        self.pending.entry(frame.from()).or_default().push(Entry {
            frame,
            state: FrameState::Tracked,
            last_hold_head: None,
        });
        self.len()
    }

    /// Total frames (tracked + tentative) at this instant.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Sender set this instant (poll iteration bounds).
    #[must_use]
    pub fn senders(&self) -> Vec<Address> {
        self.pending.keys().copied().collect()
    }

    /// The pool-known predecessor set for `sender`: every TRACKED frame's gap
    /// nonce that lies in the caller's pool window `[head_nonce, pending_count)`.
    /// The gap and the window are intersected directly, so a stale pending
    /// count (or a huge claimed gap) never materializes the full range.
    /// Sorted and deduped; degraded frames contribute nothing (they can never
    /// rescue).
    #[must_use]
    pub fn pool_known_gap(&self, sender: Address, head_nonce: u64, pending_count: u64) -> Vec<u64> {
        let mut nonces: Vec<u64> = Vec::new();
        if let Some(frames) = self.pending.get(&sender) {
            for entry in frames {
                if !matches!(entry.state, FrameState::Tracked) || entry.frame.is_degraded() {
                    continue;
                }
                let identity = entry.frame.identity();
                let start = identity.expected_at_capture.max(head_nonce);
                let end = identity.claimed_nonce.min(pending_count);
                if start < end {
                    nonces.extend(start..end);
                }
            }
        }
        nonces.sort_unstable();
        nonces.dedup();
        nonces
    }

    /// The state of the frame with `hash`, if tracked at all.
    #[must_use]
    pub fn state(&self, hash: B256) -> Option<FrameState> {
        self.pending
            .values()
            .flatten()
            .find(|entry| entry.frame.hash() == hash)
            .map(|entry| entry.state)
    }

    /// Number of tentative frames (the finality/reorg working set).
    #[must_use]
    pub fn tentative_count(&self) -> usize {
        self.pending
            .values()
            .flatten()
            .filter(|entry| matches!(entry.state, FrameState::Tentative(_)))
            .count()
    }

    /// Poll one sender's tracked frames against fresh evidence.
    ///
    /// * `head_nonce`: `eth_getTransactionCount(sender, latest)`
    /// * `pool_known_gap`: the set of gap nonces whose tx the caller can
    ///   already see (`eth_getTransactionBySenderAndNonce` +
    ///   `txpool_contentFrom` combined)
    ///
    /// Nonce semantics: `head_nonce` is `eth_getTransactionCount(latest)` -
    /// the NEXT unconsumed nonce, so a frame's slot is consumed only when
    /// headNonce > claim. Head ON the claim is the open frontier: every gap
    /// nonce is already mined, so `poll` rescues the frame with an EMPTY
    /// predecessor list (replay against the current head needs no prefix) and
    /// the frame leaves the FSM.
    ///
    /// Returns one decision per TRACKED frame of that sender (in push order).
    /// `Rescue` removes the frame (it leaves the FSM); `NonceConsumed` and
    /// `StillWaiting` keep it parked -- the caller classifies a consumed
    /// nonce via [`Self::enter_tentative`]. A Rescue whose exact predecessor
    /// list (and, for a re-park, boundary) was already proved futile is
    /// suppressed to `StillWaiting` (see `repark_guard`). A degraded frame has
    /// no wire to replay, so its decision is `WireLost` (once per head advance,
    /// pegged to the missing-wire reason) and never a rescue.
    pub fn poll(
        &mut self,
        sender: Address,
        head_nonce: u64,
        pool_known_gap: &[u64],
    ) -> Vec<(ParkedFrame, QuarantineDecision)> {
        self.poll_inner(sender, head_nonce, pool_known_gap, None)
    }

    /// As [`Self::poll`], with the pool-known predecessors' content identity
    /// (`(nonce, tx hash)`) so a guard recorded for the same nonce list but
    /// REPLACED predecessor content re-arms instead of suppressing.
    ///
    /// An empty `content` slice is no content evidence: the guard falls back
    /// to the nonce + boundary comparison, so a transient hydrate failure
    /// cannot re-fire a proved-futile rescue on every tick.
    pub fn poll_with_content(
        &mut self,
        sender: Address,
        head_nonce: u64,
        pool_known_gap: &[u64],
        content: &[(u64, B256)],
    ) -> Vec<(ParkedFrame, QuarantineDecision)> {
        self.poll_inner(sender, head_nonce, pool_known_gap, Some(content))
    }

    fn poll_inner(
        &mut self,
        sender: Address,
        head_nonce: u64,
        pool_known_gap: &[u64],
        content: Option<&[(u64, B256)]>,
    ) -> Vec<(ParkedFrame, QuarantineDecision)> {
        let Some(frames) = self.pending.remove(&sender) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(frames.len());
        let mut keep = Vec::with_capacity(frames.len());
        for mut entry in frames {
            if let FrameState::Tentative(_) = entry.state {
                keep.push(entry);
                continue;
            }
            let identity = entry.frame.identity();
            if head_nonce > identity.claimed_nonce {
                out.push((entry.frame.clone(), QuarantineDecision::NonceConsumed));
                keep.push(entry);
                continue;
            }
            if let Some(reason) = entry.frame.degraded_reason() {
                // A degraded frame has no wire to replay, so no rescue exists
                // for it. It stays quietly tracked until the chain proves the
                // claimed nonce consumed (head past the claim); the hold names
                // the missing wire once per head advance, never per poll.
                if entry.last_hold_head != Some(head_nonce) {
                    entry.last_hold_head = Some(head_nonce);
                    out.push((entry.frame.clone(), QuarantineDecision::WireLost { reason }));
                }
                keep.push(entry);
                continue;
            }
            let unknown: Vec<u64> = identity
                .gap_range()
                .into_iter()
                .filter(|n| !pool_known_gap.contains(n) && *n >= head_nonce)
                .collect();
            let preds: Vec<u64> = identity
                .gap_range()
                .into_iter()
                .filter(|n| *n >= head_nonce)
                .collect();
            if unknown.is_empty() {
                if self.rescue_is_guarded(
                    identity.hash,
                    &preds,
                    identity.expected_at_capture,
                    content,
                ) {
                    // The caller already proved an identical rescue futile
                    // (a same-boundary re-park, or a structural decode
                    // failure). Hold the frame until the predecessor list or
                    // boundary changes.
                    out.push((
                        entry.frame.clone(),
                        QuarantineDecision::StillWaiting { unknown: preds },
                    ));
                    keep.push(entry);
                } else {
                    // Every gap nonce is fetchable, so the frame leaves the FSM
                    // into the funnel. At the frontier the head sits exactly on
                    // the claim and `preds` is empty: the whole gap is already
                    // mined and replaying against the current head needs no
                    // prefix.
                    self.repark_guard.remove(&entry.frame.hash());
                    out.push((
                        entry.frame.clone(),
                        QuarantineDecision::Rescue {
                            predecessors: preds,
                        },
                    ));
                }
            } else {
                out.push((
                    entry.frame.clone(),
                    QuarantineDecision::StillWaiting { unknown },
                ));
                keep.push(entry);
            }
        }
        if !keep.is_empty() {
            self.pending.insert(sender, keep);
        }
        out
    }

    /// Whether an identical rescue for `hash` was already proved futile.
    fn rescue_is_guarded(
        &self,
        hash: B256,
        predecessors: &[u64],
        boundary: u64,
        content: Option<&[(u64, B256)]>,
    ) -> bool {
        match self.repark_guard.get(&hash) {
            Some(RescueGuard::Reparked {
                predecessors: guarded,
                content: guarded_content,
                boundary: guarded_boundary,
            }) => {
                guarded.as_slice() == predecessors
                    && *guarded_boundary == boundary
                    && content_matches(guarded_content, predecessors, content)
            }
            Some(RescueGuard::Unhydratable {
                predecessors: guarded,
                content: guarded_content,
            }) => {
                guarded.as_slice() == predecessors
                    && content_matches(guarded_content, predecessors, content)
            }
            None => false,
        }
    }

    /// Record that a `GapPending` re-park proved a rescue of this exact
    /// predecessor list against this boundary futile. The caller records this
    /// only after observing the re-park, never on the rescue decision itself.
    /// Record the predecessors' content identity alongside the predecessor
    /// list and boundary, so a later poll can tell a replaced predecessor
    /// (same nonces, new hashes) from the same futile queue.
    pub fn record_repark_guard_with_content(
        &mut self,
        frame_hash: B256,
        predecessors: &[u64],
        content: &[B256],
        boundary: u64,
    ) {
        self.repark_guard.insert(
            frame_hash,
            RescueGuard::Reparked {
                predecessors: predecessors.to_vec(),
                content: content.to_vec(),
                boundary,
            },
        );
    }

    /// Record that a predecessor of this list returned a shape that cannot
    /// decode. Permanent for the pool-pred lane: re-requesting the same list
    /// returns the same shape. The empty mined-frontier rescue is a different
    /// evidence class and stays available.
    /// Record whatever content identity the hydration could lift before it
    /// failed. An empty vec is recorded as-is and can never suppress (see
    /// [`content_matches`]).
    pub fn record_unhydratable_guard_with_content(
        &mut self,
        frame_hash: B256,
        predecessors: &[u64],
        content: &[B256],
    ) {
        self.repark_guard.insert(
            frame_hash,
            RescueGuard::Unhydratable {
                predecessors: predecessors.to_vec(),
                content: content.to_vec(),
            },
        );
    }

    /// Whether any tracked frame of `sender` carries a recorded rescue guard.
    /// The caller's signal to fetch the pool-known predecessors' content
    /// before polling, so a content-replaced predecessor can re-arm.
    #[must_use]
    pub fn has_guard_for_sender(&self, sender: Address) -> bool {
        self.pending.get(&sender).is_some_and(|frames| {
            frames
                .iter()
                .any(|entry| self.repark_guard.contains_key(&entry.frame.hash()))
        })
    }

    /// The captured boundary of the frame with `hash`; keys a re-park guard on
    /// the boundary the re-park actually carried.
    #[must_use]
    pub fn expected_boundary(&self, frame_hash: B256) -> Option<u64> {
        self.pending
            .values()
            .flatten()
            .find(|entry| entry.frame.hash() == frame_hash)
            .map(|entry| entry.frame.expected_at_capture())
    }

    /// Move a tracked frame into [`FrameState::Tentative`] with the caller's
    /// classification evidence. Returns `false` when the frame is unknown or
    /// already tentative.
    pub fn enter_tentative(&mut self, frame_hash: B256, consumed: NonceConsumed) -> bool {
        for entry in self.pending.values_mut().flatten() {
            if entry.frame.hash() == frame_hash && entry.state == FrameState::Tracked {
                entry.state = FrameState::Tentative(consumed);
                self.repark_guard.remove(&frame_hash);
                return true;
            }
        }
        false
    }

    /// Distinct `(block, block_hash)` pairs across the tentative set, in
    /// ascending block order -- the deduped reorg-check worklist.
    #[must_use]
    pub fn tentative_blocks(&self) -> Vec<(u64, B256)> {
        let mut blocks: BTreeMap<u64, B256> = BTreeMap::new();
        for entry in self.pending.values().flatten() {
            if let FrameState::Tentative(consumed) = entry.state {
                blocks.insert(consumed.block(), consumed.block_hash());
            }
        }
        blocks.into_iter().collect()
    }

    /// Reorg check for one block: every tentative frame recorded at `block`
    /// whose recorded hash does NOT match the canonical `canonical_hash`
    /// (or whose block is gone) is REVIVED back to [`FrameState::Tracked`].
    /// Returns the revived frames for logging.
    #[must_use]
    pub fn check_reorg(&mut self, block: u64, canonical_hash: Option<B256>) -> Vec<ParkedFrame> {
        let mut revived = Vec::new();
        for entry in self.pending.values_mut().flatten() {
            if let FrameState::Tentative(consumed) = entry.state {
                if consumed.block() == block && canonical_hash != Some(consumed.block_hash()) {
                    entry.state = FrameState::Tracked;
                    self.repark_guard.remove(&entry.frame.hash());
                    revived.push(entry.frame.clone());
                }
            }
        }
        revived
    }

    /// Finality sweep: every tentative frame whose consumption block is at or
    /// below `finalized_number` is removed and returned (the caller writes a
    /// tombstone). A frame above the tag stays tentative.
    pub fn check_finalized(&mut self, finalized_number: u64) -> Vec<(ParkedFrame, NonceConsumed)> {
        let mut dead = Vec::new();
        for entries in self.pending.values_mut() {
            entries.retain(|entry| {
                if let FrameState::Tentative(consumed) = entry.state {
                    if consumed.block() <= finalized_number {
                        dead.push((entry.frame.clone(), consumed));
                        return false;
                    }
                }
                true
            });
        }
        self.pending.retain(|_, entries| !entries.is_empty());
        for (frame, _) in &dead {
            self.repark_guard.remove(&frame.hash());
        }
        dead
    }

    /// Force-remove one sender's frames (operator flush). Returns them in
    /// push order (log-friendly).
    pub fn evict_sender(&mut self, sender: Address) -> Vec<ParkedFrame> {
        let frames: Vec<ParkedFrame> = self
            .pending
            .remove(&sender)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.frame)
            .collect();
        for frame in &frames {
            self.repark_guard.remove(&frame.hash());
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::b256;

    const SENDER: Address = Address::ZERO;

    const HASH: B256 = b256!("0000000000000000000000000000000000000000000000000000000000000001");

    fn event(hash: B256, nonce: u64) -> BackrunFeedEvent {
        BackrunFeedEvent {
            chain_id: 1,
            from: SENDER,
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            gas: 300_000,
            max_fee_per_gas: 514_684_409,
            max_priority_fee_per_gas: 1_000_000_000,
            nonce,
            hash,
            access_list: serde_json::json!([]),
            tx_type: 2,
            received_unix_ms: 1_700_000_000_000,
            raw_signed_tx: None,
        }
    }

    /// A complete park: every wire field came from the feed.
    fn frame(nonce: u64, expected: u64) -> ParkedFrame {
        ParkedFrame::from_event(&event(HASH, nonce), expected)
    }

    /// A complete park whose wire fields are explicitly ZERO: real values, not
    /// a degradation sentinel.
    fn zero_wire_frame(nonce: u64, expected: u64) -> ParkedFrame {
        ParkedFrame::Complete(Box::new(CompleteFrame {
            identity: FrameIdentity {
                hash: HASH,
                from: SENDER,
                claimed_nonce: nonce,
                expected_at_capture: expected,
            },
            wire: FrameWire {
                to: None,
                value: U256::ZERO,
                data: Bytes::new(),
                gas: 0,
                max_fee_per_gas: 0,
                max_priority_fee_per_gas: 0,
                chain_id: 0,
                tx_type: 0,
                access_list: serde_json::json!([]),
                received_unix_ms: 0,
                raw_signed_tx: None,
            },
        }))
    }

    /// A migrated park: identity only, no wire to replay.
    fn degraded(nonce: u64, expected: u64) -> ParkedFrame {
        ParkedFrame::degraded(
            FrameIdentity {
                hash: HASH,
                from: SENDER,
                claimed_nonce: nonce,
                expected_at_capture: expected,
            },
            MissingWire::LegacyIncomplete,
        )
    }

    fn hash(byte: u8) -> B256 {
        let mut raw = [0u8; 32];
        raw[0] = byte;
        B256::from(raw)
    }

    #[test]
    fn rescue_lists_every_predecessor_in_order() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10));
        let out = q.poll(SENDER, 10, &[10, 11]);
        let (f, decision) = &out[0];
        assert_eq!(f.claimed_nonce(), 12);
        assert_eq!(f.expected_at_capture(), 10);
        assert_eq!(
            decision,
            &QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            }
        );
        assert!(q.is_empty(), "a rescued frame leaves the FSM");
    }

    #[test]
    fn still_waiting_names_the_unknown_nonce_and_stays_parked() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10));
        let out = q.poll(SENDER, 10, &[11]);
        assert_eq!(
            out[0].1,
            QuarantineDecision::StillWaiting { unknown: vec![10] }
        );
        assert_eq!(q.len(), 1);
        // No clock exists: a frame whose predecessor never surfaces stays
        // parked on every later poll, however stale it is by wall clock.
        for _ in 0..1_000 {
            let out = q.poll(SENDER, 10, &[11]);
            assert_eq!(
                out[0].1,
                QuarantineDecision::StillWaiting { unknown: vec![10] }
            );
        }
        assert_eq!(q.len(), 1, "no TTL ever evicts a still-waiting frame");
    }

    /// The guard keys on the exact predecessor list and the frame's captured
    /// boundary. An unrelated pool transaction that only moves the count lane
    /// is not evidence, so it must not re-arm a futile rescue.
    #[test]
    fn repark_guard_suppresses_only_an_identical_list_and_boundary() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10));
        let first = q.poll(SENDER, 10, &[10, 11]);
        assert_eq!(
            first[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            }
        );
        assert!(q.is_empty(), "the rescued frame leaves the FSM");

        // The funnel re-parked on the SAME boundary; the caller records the
        // deterministic-futile signal plus the predecessors' content identity.
        let content = [(10, hash(0xa1)), (11, hash(0xa2))];
        q.record_repark_guard_with_content(
            frame(12, 10).hash(),
            &[10, 11],
            &[content[0].1, content[1].1],
            10,
        );
        q.push(frame(12, 10));
        assert_eq!(
            q.poll_with_content(SENDER, 10, &[10, 11], &content)[0].1,
            QuarantineDecision::StillWaiting {
                unknown: vec![10, 11]
            },
            "identical list, content, and boundary must not re-fire the rescue"
        );
        assert_eq!(q.len(), 1, "the suppressed frame stays parked");

        // The head advances: the predecessor list changes, so the guard no
        // longer covers the rescue and it re-fires.
        assert_eq!(
            q.poll_with_content(SENDER, 11, &[11], &[(11, hash(0xa2))])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![11]
            }
        );
        assert!(q.is_empty());
    }

    /// A guard needs content on BOTH sides: an empty stored or incoming
    /// content vec cannot prove the predecessors are unchanged, and a false
    /// suppression is a lost rescue, so the rescue re-arms.
    #[test]
    fn empty_content_on_either_side_never_suppresses() {
        let h10 = hash(0xa1);
        let h11 = hash(0xa2);

        // Stored content empty: the incoming content cannot make it match.
        let mut q = Quarantine::new();
        q.record_unhydratable_guard_with_content(frame(12, 10).hash(), &[10, 11], &[]);
        q.push(frame(12, 10));
        assert_eq!(
            q.poll_with_content(SENDER, 10, &[10, 11], &[(10, h10), (11, h11)])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            },
            "empty stored content re-arms"
        );

        // Incoming content empty (or absent): the stored content cannot match.
        let mut q = Quarantine::new();
        q.record_unhydratable_guard_with_content(frame(12, 10).hash(), &[10, 11], &[h10, h11]);
        q.push(frame(12, 10));
        assert_eq!(
            q.poll_with_content(SENDER, 10, &[10, 11], &[])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            },
            "empty incoming content re-arms"
        );

        let mut q = Quarantine::new();
        q.record_unhydratable_guard_with_content(frame(12, 10).hash(), &[10, 11], &[h10, h11]);
        q.push(frame(12, 10));
        assert_eq!(
            q.poll(SENDER, 10, &[10, 11])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            },
            "no content evidence at all re-arms"
        );
    }

    /// A transient hydration failure records no guard: without an entry the
    /// next poll re-fires the identical rescue.
    #[test]
    fn no_guard_records_means_the_rescue_re_fires() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10));
        assert!(matches!(
            q.poll(SENDER, 10, &[10, 11])[0].1,
            QuarantineDecision::Rescue { .. }
        ));
        q.push(frame(12, 10));
        assert!(matches!(
            q.poll(SENDER, 10, &[10, 11])[0].1,
            QuarantineDecision::Rescue { .. }
        ));
    }

    /// A structural decode failure suppresses only the failed pool-pred list:
    /// the empty mined-frontier rescue is a different evidence class and stays
    /// available.
    #[test]
    fn unhydratable_guard_suppresses_the_pool_pred_list_only() {
        let mut q = Quarantine::new();
        let content = [(10, hash(0xa1)), (11, hash(0xa2))];
        q.record_unhydratable_guard_with_content(
            frame(12, 10).hash(),
            &[10, 11],
            &[content[0].1, content[1].1],
        );
        q.push(frame(12, 10));
        assert_eq!(
            q.poll_with_content(SENDER, 10, &[10, 11], &content)[0].1,
            QuarantineDecision::StillWaiting {
                unknown: vec![10, 11]
            }
        );
        // Head reaches the claim: the predecessor prefix is empty and the
        // frame re-enters the funnel.
        assert_eq!(
            q.poll(SENDER, 12, &[])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![]
            }
        );
        assert!(q.is_empty());
    }

    #[test]
    fn pool_known_gap_is_bounded_to_tracked_ranges() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10));
        assert_eq!(q.pool_known_gap(SENDER, 10, 12), vec![10, 11]);
        // A stale pending count cannot widen the set past the tracked gap.
        assert_eq!(q.pool_known_gap(SENDER, 10, 1_000_000), vec![10, 11]);
        // Head advanced into the gap: only the unresolved nonce remains.
        assert_eq!(q.pool_known_gap(SENDER, 11, 12), vec![11]);
        assert_eq!(q.pool_known_gap(SENDER, 10, 10), Vec::<u64>::new());
    }

    #[test]
    fn consumed_nonce_stays_tracked_until_classified() {
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash();
        q.push(f);
        // Head past the claim: consumption, classification begins.
        let out = q.poll(SENDER, 13, &[]);
        assert_eq!(out[0].1, QuarantineDecision::NonceConsumed);
        assert_eq!(q.state(h), Some(FrameState::Tracked));
        assert!(q.enter_tentative(
            h,
            NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(7)
            }
        ));
        assert_eq!(
            q.state(h),
            Some(FrameState::Tentative(NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(7)
            }))
        );
        assert_eq!(q.len(), 1, "the frame survives into the tentative set");
    }

    #[test]
    fn finality_tombstones_only_at_or_below_the_tag() {
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash();
        q.push(f);
        let _ = q.poll(SENDER, 13, &[]);
        q.enter_tentative(
            h,
            NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(7),
            },
        );
        // Tag below the consumption block: still tentative.
        assert!(q.check_finalized(99).is_empty());
        assert_eq!(q.tentative_count(), 1);
        // Tag reaches the block: the tombstone fires exactly once.
        let dead = q.check_finalized(100);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].1.block(), 100);
        assert!(dead[0].1.mined());
        assert!(q.is_empty(), "a finalized frame leaves the FSM");
        assert!(q.check_finalized(200).is_empty(), "never tombstoned twice");
    }

    #[test]
    fn reorg_revives_on_hash_mismatch_but_not_on_match() {
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash();
        q.push(f);
        let _ = q.poll(SENDER, 13, &[]);
        q.enter_tentative(
            h,
            NonceConsumed::SlotTakenAt {
                block: 100,
                block_hash: hash(7),
                by: Some(hash(9)),
            },
        );
        // Matching canonical hash: stays tentative.
        assert!(q.check_reorg(100, Some(hash(7))).is_empty());
        assert_eq!(q.tentative_count(), 1);
        // Mismatch: revived to Tracked.
        let revived = q.check_reorg(100, Some(hash(8)));
        assert_eq!(revived.len(), 1);
        assert_eq!(q.state(h), Some(FrameState::Tracked));
        // A missing block (None) also revives.
        q.enter_tentative(
            h,
            NonceConsumed::MinedAt {
                block: 101,
                block_hash: hash(3),
            },
        );
        assert_eq!(q.check_reorg(101, None).len(), 1);
        assert_eq!(q.state(h), Some(FrameState::Tracked));
    }

    #[test]
    fn tentative_blocks_are_deduped() {
        let mut q = Quarantine::new();
        let a = frame(12, 10).with_hash(hash(1));
        let b = frame(13, 11).with_hash(hash(2));
        q.push(a);
        q.push(b);
        // Both frames share a sender, so poll once and classify both.
        for f in q.poll(SENDER, 20, &[]) {
            q.enter_tentative(
                f.0.hash(),
                NonceConsumed::MinedAt {
                    block: 100,
                    block_hash: hash(7),
                },
            );
        }
        assert_eq!(q.tentative_blocks(), vec![(100, hash(7))]);
    }

    #[test]
    fn gap_range_is_the_explicit_predecessor_order() {
        assert_eq!(frame(5, 5).gap_range(), Vec::<u64>::new());
        assert_eq!(frame(8, 5).gap_range(), vec![5, 6, 7]);
    }

    #[test]
    fn degraded_frame_never_rescues_and_holds_once_per_head_advance() {
        let mut q = Quarantine::new();
        let f = degraded(14, 12);
        assert!(f.is_degraded());
        assert!(f.complete().is_none(), "a degraded park carries no wire");
        assert_eq!(f.degraded_reason(), Some(MissingWire::LegacyIncomplete));
        let h = f.hash();
        q.push(f);
        // A degraded park contributes no gap nonce: there is no predecessor
        // lane for a frame that can never rescue.
        assert!(q.pool_known_gap(SENDER, 12, 14).is_empty());
        // head 12: one hold surfaces, never a rescue - even with the whole gap
        // in the pool view.
        let first = q.poll(SENDER, 12, &[12, 13]);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].1,
            QuarantineDecision::WireLost {
                reason: MissingWire::LegacyIncomplete
            }
        );
        // Same head: quiet, no repeat.
        assert!(q.poll(SENDER, 12, &[12, 13]).is_empty());
        // Head advance: one more hold.
        assert_eq!(q.poll(SENDER, 13, &[12, 13]).len(), 1);
        assert!(q.poll(SENDER, 13, &[12, 13]).is_empty());
        assert_eq!(q.state(h), Some(FrameState::Tracked));
        // Head past the claim: chain proof, not a rescue.
        let consumed = q.poll(SENDER, 15, &[]);
        assert_eq!(consumed[0].1, QuarantineDecision::NonceConsumed);
    }

    /// Wire completeness, not a sentinel value, decides the variant: a frame
    /// whose real wire fields are zero is complete and rescues like any other.
    #[test]
    fn complete_frame_with_real_zero_values_still_rescues() {
        let mut q = Quarantine::new();
        let f = zero_wire_frame(12, 10);
        assert!(
            !f.is_degraded(),
            "zero values are values, never degradation"
        );
        assert_eq!(f.degraded_reason(), None);
        q.push(f);
        assert_eq!(
            q.poll(SENDER, 10, &[10, 11])[0].1,
            QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            }
        );
        assert!(q.is_empty(), "a complete frame leaves through the funnel");
    }

    /// The funnel's event rebuild exists only on the complete arm, so a
    /// degraded park can never fabricate one.
    #[test]
    fn event_rebuild_exists_only_for_complete_frames() {
        let complete = frame(12, 10);
        let event = complete.complete().map(CompleteFrame::to_event);
        assert!(event.is_some(), "a complete park rebuilds its feed event");
        assert_eq!(event.map(|e| e.nonce), Some(12));

        let degraded = degraded(12, 10);
        assert!(degraded.complete().is_none(), "no wire, no event");
        assert_eq!(
            degraded.degraded_reason(),
            Some(MissingWire::LegacyIncomplete)
        );
    }

    /// A degraded frame's life is the chain's to end: consumption, reorg
    /// revival, then finality -- identical to a complete frame's path.
    #[test]
    fn degraded_frame_walks_the_finality_and_reorg_paths() {
        let mut q = Quarantine::new();
        let f = degraded(12, 10);
        let h = f.hash();
        q.push(f);
        assert_eq!(
            q.poll(SENDER, 13, &[])[0].1,
            QuarantineDecision::NonceConsumed
        );
        assert!(q.enter_tentative(
            h,
            NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(7),
            }
        ));
        assert!(q.check_finalized(99).is_empty());
        // A reorg revives it exactly like a complete frame: the tombstone had
        // no wire to lose.
        assert_eq!(q.check_reorg(100, Some(hash(8))).len(), 1);
        assert_eq!(q.state(h), Some(FrameState::Tracked));
        assert!(q.enter_tentative(
            h,
            NonceConsumed::MinedAt {
                block: 100,
                block_hash: hash(8),
            }
        ));
        let dead = q.check_finalized(100);
        assert_eq!(dead.len(), 1);
        assert!(dead[0].0.is_degraded(), "the tombstone keeps the identity");
        assert!(q.is_empty(), "a finalized frame leaves the FSM");
    }

    #[test]
    fn no_pool_absence_ever_evicts() {
        // Pool lanes are advisory: `poll` with an empty pool view must return
        // `StillWaiting`, never a death. There is no code path from pool
        // evidence to eviction -- only finality tombstones and rescue remove.
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash();
        q.push(f);
        for _ in 0..100 {
            let out = q.poll(SENDER, 10, &[]);
            assert!(matches!(out[0].1, QuarantineDecision::StillWaiting { .. }));
        }
        assert_eq!(q.state(h), Some(FrameState::Tracked));
    }
}
