//! Keyed registration-gate table for immutable V4 admission verdicts
//! (PRG-2).
//!
//! `BotState` itself owns the gate: when `register_v4_pool` refuses a pool
//! on an IMMUTABLE pool fact (dynamic fee, static fee exceeding the
//! executor's 2-byte encoding limit), the verdict is recorded keyed by
//! `(pool_manager, pool_id)`. Every later registration-candidate pass —
//! the crawl re-offers the same pools block after block — consults the gate
//! BEFORE any RPC work on the registration path (the construction door's V4
//! get half), short-circuiting the resolve + slot0/liquidity fetch +
//! tick-map assembly with the same typed refusal.
//!
//! This dissolves the retired Python `SkipGate` fatal memo: Rust-computed
//! verdicts no longer cross the FFI to be memoized on the Python heap and
//! re-consulted (~20k skip-checks/block at mainnet scale). Raced duplicates
//! and transient RPC errors are NEVER recorded — they are not pool facts
//! .
//!
//! Hooked pools are NOT gate-recorded: per ADR-037 they are admitted
//! with a simulation caveat (the `HookedPool` variant is reserved).

use alloy::primitives::Address;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::{BuildFailure, RegisterRefusal};
use hashbrown::HashMap;

/// An immutable V4 admission refusal (PRG-2 gate). Mirrors the fee-fact
/// variants of [`crate::RegisterV4PoolError`] minus the transient/wiring
/// ones (already-registered, spec violation) which are never pool facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionVerdict {
    /// Pool uses a dynamic fee (`fee == 0x0080_0000`) — can never carry a
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

impl From<AdmissionVerdict> for RegisterRefusal {
    /// The verdict's ONE rendering in the write door's refusal vocabulary: a
    /// pre-RPC consult answers exactly the refusal registering the pool would
    /// have produced, so the boundary's exception surface never branches on
    /// which path saw the fact first.
    fn from(verdict: AdmissionVerdict) -> Self {
        match verdict {
            AdmissionVerdict::DynamicFee { fee } => Self::Admission {
                failure: BuildFailure::DynamicFee,
                hook_flags: None,
                fee: Some(fee),
            },
            AdmissionVerdict::FeeExceedsEncoderLimit { fee } => Self::Admission {
                failure: BuildFailure::HighFee,
                hook_flags: None,
                fee: Some(fee),
            },
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
        gate.record(pm, pid, AdmissionVerdict::DynamicFee { fee: 0x0080_0000 });

        let v = gate.verdict(pm, pid).expect("recorded verdict answers");
        assert_eq!(v, AdmissionVerdict::DynamicFee { fee: 0x0080_0000 });
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

    /// The verdict renders the write door's own admission refusal — the same
    /// two arms `From<&RegisterV4PoolError>` produces for the identical facts,
    /// so a zero-I/O consult cannot answer a different refusal than the
    /// registration it stands in for.
    #[test]
    fn a_verdict_renders_the_write_doors_admission_refusal() {
        assert_eq!(
            RegisterRefusal::from(AdmissionVerdict::FeeExceedsEncoderLimit { fee: 320_000 }),
            RegisterRefusal::from(&crate::RegisterV4PoolError::FeeExceedsEncoderLimit {
                fee: 320_000
            }),
        );
        assert_eq!(
            RegisterRefusal::from(AdmissionVerdict::DynamicFee { fee: 0x0080_0000 }),
            RegisterRefusal::from(&crate::RegisterV4PoolError::DynamicFee { fee: 0x0080_0000 }),
        );
    }
}
