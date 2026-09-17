//! Gap-quarantine (ergo 6PDJJR): wire frames whose claimed nonce sits ahead
//! of the parent state's sender nonce park here instead of dropping. Each
//! poll tick carries the freshest (head state, pool knowledge) evidence; the
//! FSM answers per parked frame — rescue (replay predecessors first), wait,
//! close-by-head (head state passed the frame — replay plain), evict (TTL
//! exceeded or a gap nonce stayed unknowable for too long / broke).
//!
//! The core is I/O-free: the caller (the sidecar bin's frame loop) fetches
//! pool/head evidence and performs actual replay/hydration. This keeps the
//! FSM unit-testable without a node.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, U256};

/// A frame parked at its gap boundary, carrying the wire fields a flip-side
/// replay needs when the gap closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedFrame {
    pub hash: alloy::primitives::B256,
    pub from: Address,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// The nonce the wire claims for this frame (the gap's TOP edge).
    pub claimed_nonce: u64,
    /// Received wall-clock — the TTL runs off this, neither relay
    /// nor mempool delay can out-run it.
    pub arrived_at: Instant,
    /// The head-state account nonce at capture (`expected`); the predecessor
    /// set is `[expected, claimed)`.
    pub expected_at_capture: u64,
}

impl ParkedFrame {
    /// Gap nonces that must land before the frame validates.
    #[must_use]
    pub fn gap_range(&self) -> Vec<u64> {
        (self.expected_at_capture..self.claimed_nonce).collect()
    }
}

/// What one poll-cycle says about one parked frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineDecision {
    /// Every gap nonce is fetchable from the caller's pool view: hydrate the
    /// predecessors (in the returned ascending order) into the scratch EVM,
    /// then the frame.
    Rescue {
        /// Predecessor nonces to hydrate first, ascending.
        predecessors: Vec<u64>,
    },
    /// The head state already covered the gap (the predecessors settled
    /// on-chain) — replay the frame plain.
    ClosedByHead,
    /// Nothing changed since capture: keep the frame parked.
    StillWaiting {
        /// Gap nonces whose tx the pool view still cannot produce.
        unknown: Vec<u64>,
    },
    /// The TTL exceeded — drop with truthful reason `gap_expired`.
    GapExpired,
}

/// Quarantine limits identical across senders.
#[derive(Debug, Clone, Copy)]
pub struct QuarantinePolicy {
    /// Frames parked longer than this are evicted. Default = 24s (two slots).
    pub ttl: Duration,
    /// Per-sender parked-frame cap — FIFO beyond the cap evicts the oldest.
    pub per_sender_cap: usize,
}

impl Default for QuarantinePolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(24),
            per_sender_cap: 64,
        }
    }
}

/// The pure FSM core. Per-sender FIFOs of parked frames; `poll` returns one
/// decision per parked frame of that sender; non-ready frames stay parked.
#[derive(Debug, Default)]
pub struct Quarantine {
    pending: BTreeMap<Address, Vec<ParkedFrame>>,
    policy: QuarantinePolicy,
}

impl Quarantine {
    /// Fresh FSM with the default policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a frame. Returns the total parked count after insert.
    pub fn push(&mut self, frame: ParkedFrame) -> usize {
        let entry = self.pending.entry(frame.from).or_default();
        entry.push(frame);
        let excess = entry.len().saturating_sub(self.policy.per_sender_cap);
        for _ in 0..excess {
            entry.remove(0);
        }
        self.len()
    }

    /// Total parked frames at this instant.
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

    /// Poll one (sender, nonce-bucket) against fresh evidence.
    ///
    /// * `head_nonce`: `eth_getTransactionCount(sender, latest)`
    /// * `pool_known_gap`: the set of gap nonces whose tx the caller can
    ///   already see (`eth_getTransactionBySenderAndNonce`+
    ///   `txpool_contentFrom` combined)
    ///
    /// Returns one decision per parked frame of that sender (in push order);
    /// non-evicted entries stay parked in FIFO position.
    pub fn poll(
        &mut self,
        sender: Address,
        head_nonce: u64,
        pool_known_gap: &[u64],
        now: Instant,
    ) -> Vec<(ParkedFrame, QuarantineDecision)> {
        let Some(frames) = self.pending.remove(&sender) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(frames.len());
        let mut still = Vec::new();
        for frame in frames {
            let decision = if now.duration_since(frame.arrived_at) > self.policy.ttl {
                QuarantineDecision::GapExpired
            } else if head_nonce >= frame.claimed_nonce {
                QuarantineDecision::ClosedByHead
            } else {
                let unknown: Vec<u64> = frame
                    .gap_range()
                    .into_iter()
                    .filter(|n| !pool_known_gap.contains(n) && *n >= head_nonce)
                    .collect();
                let preds: Vec<u64> = frame
                    .gap_range()
                    .into_iter()
                    .filter(|n| *n >= head_nonce)
                    .collect();
                if unknown.is_empty() {
                    if preds.is_empty() {
                        QuarantineDecision::ClosedByHead
                    } else {
                        QuarantineDecision::Rescue {
                            predecessors: preds,
                        }
                    }
                } else {
                    let decision = QuarantineDecision::StillWaiting { unknown };
                    out.push((frame.clone(), decision));
                    still.push(frame);
                    continue;
                }
            };
            out.push((frame, decision));
        }
        if !still.is_empty() {
            self.pending.insert(sender, still);
        }
        out
    }

    /// Force-evict one sender's parked set. Returns the evicted frames in
    /// push order (log-friendly).
    pub fn evict_sender(&mut self, sender: Address) -> Vec<ParkedFrame> {
        self.pending.remove(&sender).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::b256;

    const SENDER: Address = Address::ZERO;

    fn frame(nonce: u64, expected: u64, arrived_back_by: Duration) -> ParkedFrame {
        ParkedFrame {
            hash: b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            from: SENDER,
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            gas: 300_000,
            max_fee_per_gas: 514_684_409,
            max_priority_fee_per_gas: 1_000_000_000,
            claimed_nonce: nonce,
            arrived_at: Instant::now()
                .checked_sub(arrived_back_by)
                .unwrap_or_else(Instant::now),
            expected_at_capture: expected,
        }
    }

    #[test]
    fn rescue_lists_every_predecessor_in_order() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10, Duration::ZERO));
        let out = q.poll(SENDER, 10, &[10, 11], Instant::now());
        let (f, decision) = &out[0];
        assert_eq!(f.claimed_nonce, 12);
        assert_eq!(f.expected_at_capture, 10);
        assert_eq!(
            decision,
            &QuarantineDecision::Rescue {
                predecessors: vec![10, 11]
            }
        );
    }

    #[test]
    fn still_waiting_names_the_unknown_nonce_and_stays_parked() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10, Duration::ZERO));
        let out = q.poll(SENDER, 10, &[11], Instant::now());
        assert_eq!(
            out[0].1,
            QuarantineDecision::StillWaiting { unknown: vec![10] }
        );
        assert_eq!(q.len(), 1);
        // A closer head moves the answer.
        let out = q.poll(SENDER, 12, &[], Instant::now());
        assert_eq!(out[0].1, QuarantineDecision::ClosedByHead);
        assert!(q.is_empty());
    }

    #[test]
    fn ttl_evicts_stale_parks() {
        let mut q = Quarantine::new();
        q.push(frame(12, 10, Duration::from_secs(30)));
        let out = q.poll(SENDER, 10, &[], Instant::now());
        assert_eq!(out[0].1, QuarantineDecision::GapExpired);
        assert!(q.is_empty());
    }

    #[test]
    fn gap_range_is_the_explicit_predecessor_order() {
        assert_eq!(frame(5, 5, Duration::ZERO).gap_range(), Vec::<u64>::new());
        assert_eq!(frame(8, 5, Duration::ZERO).gap_range(), vec![5, 6, 7]);
    }
}
