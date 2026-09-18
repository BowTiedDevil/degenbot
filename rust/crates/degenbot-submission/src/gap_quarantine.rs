//! Frame-liveness FSM (CMJ2DQ): wire frames whose claimed nonce sits ahead
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

/// A frame parked at its gap boundary, carrying the wire fields a flip-side
/// replay needs when the gap closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedFrame {
    pub hash: B256,
    pub from: Address,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// The nonce the wire claims for this frame (the gap's TOP edge).
    pub claimed_nonce: u64,
    /// The head-state account nonce at capture (`expected`); the predecessor
    /// set is `[expected, claimed)`.
    pub expected_at_capture: u64,
    /// Chain the frame was observed on (funnel re-entry wire field).
    pub chain_id: u64,
    /// EIP-2718 transaction type (funnel re-entry wire field).
    pub tx_type: u8,
    /// Access list, verbatim (funnel re-entry wire field).
    pub access_list: serde_json::Value,
    /// Feed receive time; forensics only, no liveness clock rides the frame.
    pub received_unix_ms: u64,
}

impl ParkedFrame {
    /// Gap nonces that must land before the frame validates.
    #[must_use]
    pub fn gap_range(&self) -> Vec<u64> {
        (self.expected_at_capture..self.claimed_nonce).collect()
    }

    /// Whether this frame lost its wire to a degraded journal migration. A
    /// degraded frame carries only identity (its hash, nonce, and the sender
    /// when readable); the zeroed `chain_id`/`gas` distinguish it because a
    /// real feed frame always carries both. It can never rebuild its event, so
    /// `poll` never rescues it into the funnel.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.chain_id == 0 && self.gas == 0
    }

    /// Rebuild the feed event a funnel re-entry needs. Infallible: every wire
    /// field is already typed on the frame (the journal read path parses the
    /// hex strings before constructing the frame).
    #[must_use]
    pub fn to_event(&self) -> BackrunFeedEvent {
        BackrunFeedEvent {
            chain_id: self.chain_id,
            from: self.from,
            to: self.to,
            value: self.value,
            data: self.data.clone(),
            gas: self.gas,
            max_fee_per_gas: self.max_fee_per_gas,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas,
            nonce: self.claimed_nonce,
            hash: self.hash,
            access_list: self.access_list.clone(),
            tx_type: self.tx_type,
            received_unix_ms: self.received_unix_ms,
        }
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
}

/// The pure FSM core: per-sender FIFOs of tracked and tentative frames.
/// `poll` returns one decision per tracked frame of that sender; non-rescued
/// frames stay parked.
#[derive(Debug, Default)]
pub struct Quarantine {
    pending: BTreeMap<Address, Vec<Entry>>,
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
        self.pending.entry(frame.from).or_default().push(Entry {
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

    /// The state of the frame with `hash`, if tracked at all.
    #[must_use]
    pub fn state(&self, hash: B256) -> Option<FrameState> {
        self.pending
            .values()
            .flatten()
            .find(|entry| entry.frame.hash == hash)
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
    /// nonce via [`Self::enter_tentative`].
    pub fn poll(
        &mut self,
        sender: Address,
        head_nonce: u64,
        pool_known_gap: &[u64],
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
            if entry.frame.is_degraded() {
                // A degraded frame has no wire to replay, so it can never
                // rescue. It stays quietly tracked until the chain proves the
                // claimed nonce consumed (head past the claim); the hold is
                // reported once per head advance, never per poll.
                if head_nonce > entry.frame.claimed_nonce {
                    out.push((entry.frame.clone(), QuarantineDecision::NonceConsumed));
                    keep.push(entry);
                } else if entry.last_hold_head != Some(head_nonce) {
                    entry.last_hold_head = Some(head_nonce);
                    let unknown: Vec<u64> = entry
                        .frame
                        .gap_range()
                        .into_iter()
                        .filter(|n| !pool_known_gap.contains(n) && *n >= head_nonce)
                        .collect();
                    out.push((
                        entry.frame.clone(),
                        QuarantineDecision::StillWaiting { unknown },
                    ));
                    keep.push(entry);
                } else {
                    keep.push(entry);
                }
                continue;
            }
            if head_nonce > entry.frame.claimed_nonce {
                out.push((entry.frame.clone(), QuarantineDecision::NonceConsumed));
                keep.push(entry);
                continue;
            }
            let unknown: Vec<u64> = entry
                .frame
                .gap_range()
                .into_iter()
                .filter(|n| !pool_known_gap.contains(n) && *n >= head_nonce)
                .collect();
            let preds: Vec<u64> = entry
                .frame
                .gap_range()
                .into_iter()
                .filter(|n| *n >= head_nonce)
                .collect();
            if unknown.is_empty() {
                // Every gap nonce is fetchable, so the frame leaves the FSM
                // into the funnel. At the frontier the head sits exactly on
                // the claim and `preds` is empty: the whole gap is already
                // mined and replaying against the current head needs no
                // prefix. A re-park of the same hash can only follow a
                // genuinely NEW gap captured against a later head - at
                // count == claim the replay cannot emit NonceTooHigh, so no
                // same-head rescue/park ping-pong is possible.
                out.push((
                    entry.frame.clone(),
                    QuarantineDecision::Rescue {
                        predecessors: preds,
                    },
                ));
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

    /// Move a tracked frame into [`FrameState::Tentative`] with the caller's
    /// classification evidence. Returns `false` when the frame is unknown or
    /// already tentative.
    pub fn enter_tentative(&mut self, frame_hash: B256, consumed: NonceConsumed) -> bool {
        for entry in self.pending.values_mut().flatten() {
            if entry.frame.hash == frame_hash && entry.state == FrameState::Tracked {
                entry.state = FrameState::Tentative(consumed);
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
        dead
    }

    /// Force-remove one sender's frames (operator flush). Returns them in
    /// push order (log-friendly).
    pub fn evict_sender(&mut self, sender: Address) -> Vec<ParkedFrame> {
        self.pending
            .remove(&sender)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.frame)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::b256;

    const SENDER: Address = Address::ZERO;

    fn frame(nonce: u64, expected: u64) -> ParkedFrame {
        ParkedFrame {
            hash: b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            chain_id: 1,
            from: SENDER,
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            gas: 300_000,
            max_fee_per_gas: 514_684_409,
            max_priority_fee_per_gas: 1_000_000_000,
            claimed_nonce: nonce,
            expected_at_capture: expected,
            tx_type: 2,
            access_list: serde_json::json!([]),
            received_unix_ms: 1_700_000_000_000,
        }
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
        assert_eq!(f.claimed_nonce, 12);
        assert_eq!(f.expected_at_capture, 10);
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

    #[test]
    fn consumed_nonce_stays_tracked_until_classified() {
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash;
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
        let h = f.hash;
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
        let h = f.hash;
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
        let mut a = frame(12, 10);
        a.hash = hash(1);
        let mut b = frame(13, 11);
        b.hash = hash(2);
        q.push(a);
        q.push(b);
        // Both frames share a sender, so poll once and classify both.
        for f in q.poll(SENDER, 20, &[]) {
            q.enter_tentative(
                f.0.hash,
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
        let mut f = frame(14, 12);
        f.chain_id = 0;
        f.gas = 0;
        assert!(f.is_degraded());
        let h = f.hash;
        q.push(f);
        // head 12: one hold surfaces, never a rescue.
        let first = q.poll(SENDER, 12, &[]);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].1,
            QuarantineDecision::StillWaiting {
                unknown: vec![12, 13]
            }
        );
        // Same head: quiet, no repeat.
        assert!(q.poll(SENDER, 12, &[]).is_empty());
        // Head advance: one more hold.
        assert_eq!(q.poll(SENDER, 13, &[]).len(), 1);
        assert!(q.poll(SENDER, 13, &[]).is_empty());
        assert_eq!(q.state(h), Some(FrameState::Tracked));
        // Head past the claim: chain proof, not a rescue.
        let consumed = q.poll(SENDER, 15, &[]);
        assert_eq!(consumed[0].1, QuarantineDecision::NonceConsumed);
    }

    #[test]
    fn no_pool_absence_ever_evicts() {
        // Pool lanes are advisory: `poll` with an empty pool view must return
        // `StillWaiting`, never a death. There is no code path from pool
        // evidence to eviction -- only finality tombstones and rescue remove.
        let mut q = Quarantine::new();
        let f = frame(12, 10);
        let h = f.hash;
        q.push(f);
        for _ in 0..100 {
            let out = q.poll(SENDER, 10, &[]);
            assert!(matches!(out[0].1, QuarantineDecision::StillWaiting { .. }));
        }
        assert_eq!(q.state(h), Some(FrameState::Tracked));
    }
}
