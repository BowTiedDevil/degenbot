//! Keyed registration-gate table for immutable V4 admission verdicts
//! (PRG-2).
//!
//! `BotState` itself owns the gate: when `register_v4_pool` refuses a pool
//! on an IMMUTABLE pool fact (dynamic fee, static fee exceeding the
//! executor's 2-byte encoding limit), the verdict is recorded keyed by
//! `(pool_manager, pool_id)`. Every later registration-candidate pass —
//! the crawl re-offers the same pools block after block — consults the gate
//! BEFORE any RPC work on the registration path (the `PyO3` `build_v4_pool`
//! pre-check), short-circuiting the resolve + slot0/liquidity fetch +
//! tick-map assembly with the same typed refusal.
//!
//! This dissolves the 2CBDPR-era Python `SkipGate` fatal memo: Rust-computed
//! verdicts no longer cross the FFI to be memoized on the Python heap and
//! re-consulted (~20k skip-checks/block at mainnet scale). Raced duplicates
//! and transient RPC errors are NEVER recorded — they are not pool facts
//! (CXKACI semantics).
//!
//! Hooked pools are NOT gate-recorded: per ADR-037 they are admitted
//! with a simulation caveat (the `HookedPool` variant is reserved).

use alloy::primitives::Address;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use hashbrown::HashMap;

/// An immutable V4 admission refusal (PRG-2 gate). Mirrors the fee-fact
/// variants of [`crate::RegisterV4PoolError`] minus the transient/wiring
/// ones (already-registered, spec violation) which are never pool facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionVerdict {
    /// Pool uses a dynamic fee (`fee == 0x100000`) — can never carry a
    /// static fee, so the fact is immutable.
    DynamicFee { fee: u32 },
    /// Pool's static fee exceeds the `cmd_executor`'s 2-byte encoding
    /// limit — the fee is protocol-valid but un-encodable and unprofitable;
    /// static pool-key fees never change.
    FeeExceedsEncoderLimit { fee: u32 },
}

impl AdmissionVerdict {
    /// The closed-set metric label for the verdict (instruments cardinality
    /// discipline: small closed sets only).
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::DynamicFee { .. } => "dynamic-fee",
            Self::FeeExceedsEncoderLimit { .. } => "fee-encoder-limit",
        }
    }
}

/// Keyed gate of immutable admission verdicts. Lives on `BotState`; bounded
/// by pools actually refused on admission (a small subset of the pool
/// universe), not by candidates observed.
#[derive(Default)]
pub struct RegistrationGate {
    verdicts: HashMap<(Address, V4PoolId), AdmissionVerdict>,
}

impl RegistrationGate {
    /// Record an immutable admission refusal.
    pub fn record(&mut self, pool_manager: Address, pool_id: V4PoolId, verdict: AdmissionVerdict) {
        self.verdicts.insert((pool_manager, pool_id), verdict);
    }

    /// The recorded verdict for a pool, if any.
    #[must_use]
    pub fn verdict(&self, pool_manager: Address, pool_id: V4PoolId) -> Option<AdmissionVerdict> {
        self.verdicts.get(&(pool_manager, pool_id)).copied()
    }

    /// The number of recorded verdicts (the registration-gate census).
    #[must_use]
    pub fn len(&self) -> usize {
        self.verdicts.len()
    }

    /// True when no verdict is recorded (empty-gate tripwire for tests).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use super::*;

    #[test]
    fn recorded_verdicts_are_immutable_and_keyed_by_pool() {
        let mut gate = RegistrationGate::default();
        assert!(gate.is_empty());

        let pm = Address::from([0x44u8; 20]);
        let pid: V4PoolId = [0xeeu8; 32];
        gate.record(pm, pid, AdmissionVerdict::DynamicFee { fee: 0x100_000 });

        let v = gate.verdict(pm, pid).expect("recorded verdict answers");
        assert_eq!(v, AdmissionVerdict::DynamicFee { fee: 0x100_000 });
        assert_eq!(v.kind(), "dynamic-fee");
        assert_eq!(gate.len(), 1);

        // A different pool id / manager does not answer.
        assert_eq!(gate.verdict(pm, [0x11u8; 32]), None);
        assert_eq!(gate.verdict(Address::from([0x45u8; 20]), pid), None);

        // Re-recording overwrites (same immutable fact, richer detail).
        gate.record(
            pm,
            pid,
            AdmissionVerdict::FeeExceedsEncoderLimit { fee: 320_000 },
        );
        assert_eq!(
            gate.verdict(pm, pid),
            Some(AdmissionVerdict::FeeExceedsEncoderLimit { fee: 320_000 })
        );
        assert_eq!(gate.len(), 1, "same key does not grow the table");
        assert_eq!(
            gate.verdict(pm, pid).expect("v").kind(),
            "fee-encoder-limit"
        );
    }
}
